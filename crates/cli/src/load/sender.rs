//! Sends txs with `author_submitExtrinsic` over several WebSocket connections, round robin,
//! without waiting for one tx before sending the next. Plain JSON-RPC, so the hot path skips
//! subxt. The sender holds no tx state: the tracker picks request ids and gets every reply back
//! on one channel.
//!
//! A connection counts as backed up with 4 MiB queued and not yet written (as TS
//! `bufferedAmount`), so "pool intake" fires at the same point whatever the tx size.
//!
//! A connection the node or the network closes is opened again, with backoff: the requests it
//! had written and not seen answered come back as [`Reply::Interrupted`] (their outcome is
//! unknown, and the tracker keeps watching blocks for them), and the ones still queued are
//! written on the new socket. While a connection is down, submits go to the others. Only when
//! every reconnect attempt fails does it end with [`Reply::Closed`], which the "node down" rule
//! reads.

use std::{
	collections::HashSet,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use polkameter_plugin_sdk::observers::files::Millis;
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use polkameter_plugin_sdk::chain::hex0x;

use crate::load::{now_ms, submit::Submit};

const MAX_QUEUED_BYTES: u64 = 4 * 1024 * 1024;

/// Waits before each reconnect attempt: about 15 s in all, then the node counts as down.
pub const RECONNECT_BACKOFF: [Duration; 6] = [
	Duration::from_millis(250),
	Duration::from_millis(500),
	Duration::from_secs(1),
	Duration::from_secs(2),
	Duration::from_secs(4),
	Duration::from_secs(8),
];

/// What came back for one request, or a connection that closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
	/// The node accepted the tx.
	Accepted {
		/// Request id.
		id: u64,
		/// When the reply arrived.
		at: Millis,
	},
	/// The node refused it.
	Refused {
		/// Request id.
		id: u64,
		/// When the reply arrived.
		at: Millis,
		/// JSON-RPC error code.
		code: i64,
		/// Message and data.
		error: String,
	},
	/// A connection dropped and is being opened again: the requests it had written and not seen
	/// answered have an unknown outcome. Not a node failure.
	Interrupted {
		/// Connection index.
		connection: usize,
		/// The unanswered request ids.
		ids: Vec<u64>,
		/// Why it dropped.
		reason: String,
	},
	/// A dropped connection is open again.
	Reconnected {
		/// Connection index.
		connection: usize,
		/// How long it was down.
		down_ms: Millis,
	},
	/// A connection closed for good (once per connection); its unanswered requests are lost.
	Closed {
		/// Connection index.
		connection: usize,
		/// True when it could not be opened again: the node is down. False when we closed it.
		by_node: bool,
	},
}

struct Connection {
	queue: mpsc::UnboundedSender<(u64, String)>,
	queued: Arc<AtomicU64>,
	up: Arc<AtomicBool>,
}

/// The sending side.
pub struct Sender {
	connections: Vec<Connection>,
	next: usize,
	closing: Arc<AtomicBool>,
}

/// A connection could not be opened: a wrong URL or a node that is down before the run.
#[derive(Debug, thiserror::Error)]
#[error("sender: cannot connect to {url}: {source}")]
pub struct ConnectError {
	url: String,
	source: tokio_tungstenite::tungstenite::Error,
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

impl Sender {
	/// Opens `connections` WebSockets to `url`; replies go to `replies`.
	pub async fn open(
		url: &str,
		connections: usize,
		replies: mpsc::UnboundedSender<Reply>,
	) -> Result<Self, ConnectError> {
		Self::open_with(url, connections, replies, &RECONNECT_BACKOFF).await
	}

	/// [`Sender::open`], with the waits before each reconnect attempt.
	pub async fn open_with(
		url: &str,
		connections: usize,
		replies: mpsc::UnboundedSender<Reply>,
		backoff: &[Duration],
	) -> Result<Self, ConnectError> {
		let closing = Arc::new(AtomicBool::new(false));
		let mut out = Vec::with_capacity(connections);
		for connection in 0..connections {
			let (ws, _) = tokio_tungstenite::connect_async(url)
				.await
				.map_err(|source| ConnectError { url: url.to_owned(), source })?;
			let (queue, rx) = mpsc::unbounded_channel();
			let queued = Arc::new(AtomicU64::new(0));
			let up = Arc::new(AtomicBool::new(true));
			let link = Link {
				url: url.to_owned(),
				connection,
				queued: queued.clone(),
				up: up.clone(),
				replies: replies.clone(),
				closing: closing.clone(),
				backoff: backoff.to_vec(),
			};
			tokio::spawn(link.run(ws, rx));
			out.push(Connection { queue, queued, up });
		}
		Ok(Self { connections: out, next: 0, closing })
	}

	/// Closes every connection once its queue is written.
	pub fn close(self) {
		self.closing.store(true, Ordering::Relaxed);
	}
}

/// One connection's task: writes its queue, reads replies, and opens the socket again when it
/// drops.
struct Link {
	url: String,
	connection: usize,
	queued: Arc<AtomicU64>,
	up: Arc<AtomicBool>,
	replies: mpsc::UnboundedSender<Reply>,
	closing: Arc<AtomicBool>,
	backoff: Vec<Duration>,
}

/// How a socket's session ended.
enum End {
	/// Our queue was dropped and written out: our own close.
	Done,
	/// The socket dropped.
	Dropped(String),
}

impl Link {
	async fn run(self, mut ws: Socket, mut rx: mpsc::UnboundedReceiver<(u64, String)>) {
		loop {
			let reason = match self.session(ws, &mut rx).await {
				(End::Done, _) => {
					let _ = self
						.replies
						.send(Reply::Closed { connection: self.connection, by_node: false });
					return;
				},
				(End::Dropped(_), _) if self.closing.load(Ordering::Relaxed) => {
					let _ = self
						.replies
						.send(Reply::Closed { connection: self.connection, by_node: false });
					return;
				},
				(End::Dropped(reason), ids) => {
					let _ = self.replies.send(Reply::Interrupted {
						connection: self.connection,
						ids,
						reason: reason.clone(),
					});
					reason
				},
			};
			self.up.store(false, Ordering::Relaxed);
			let down_at = now_ms();
			match self.reconnect().await {
				Some(socket) => {
					ws = socket;
					self.up.store(true, Ordering::Relaxed);
					let _ = self.replies.send(Reply::Reconnected {
						connection: self.connection,
						down_ms: now_ms().saturating_sub(down_at),
					});
				},
				None => {
					eprintln!(
						"sender: connection {} dropped ({reason}) and could not be opened again",
						self.connection
					);
					let _ = self
						.replies
						.send(Reply::Closed { connection: self.connection, by_node: true });
					return;
				},
			}
		}
	}

	/// Opens the socket again, waiting before each attempt; `None` when every attempt failed.
	async fn reconnect(&self) -> Option<Socket> {
		for wait in &self.backoff {
			tokio::time::sleep(*wait).await;
			if self.closing.load(Ordering::Relaxed) {
				return None;
			}
			if let Ok((ws, _)) = tokio_tungstenite::connect_async(&self.url).await {
				return Some(ws);
			}
		}
		None
	}

	/// Writes the queue and reads replies until the socket drops or the queue is done. Returns
	/// how it ended and the ids written and not answered.
	async fn session(
		&self,
		ws: Socket,
		rx: &mut mpsc::UnboundedReceiver<(u64, String)>,
	) -> (End, Vec<u64>) {
		let (mut sink, mut stream) = ws.split();
		let pending: Arc<Mutex<HashSet<u64>>> = Arc::default();
		let (reader_pending, replies) = (pending.clone(), self.replies.clone());
		// Reading runs apart from writing, so a slow write never stops replies being read.
		let mut reader = tokio::spawn(async move {
			while let Some(msg) = stream.next().await {
				match msg {
					Ok(Message::Text(text)) => {
						if let Some(r) = parse_reply(&text) {
							if let Reply::Accepted { id, .. } | Reply::Refused { id, .. } = &r {
								reader_pending.lock().expect("pending").remove(id);
							}
							let _ = replies.send(r);
						}
					},
					Ok(Message::Close(frame)) => {
						return frame.map_or("closed".into(), |f| format!("closed: {f}"));
					},
					Err(e) => return e.to_string(),
					Ok(_) => {}, // ping, pong, binary: the node pings every 30 s
				}
			}
			"the socket ended".to_owned()
		});
		let end = loop {
			tokio::select! {
				reason = &mut reader => break End::Dropped(reason.unwrap_or_else(|e| e.to_string())),
				item = rx.recv() => {
					let Some((id, text)) = item else {
						let _ = sink.close().await; // the queue was dropped: our own close
						// Replies to what was written still arrive until the node closes.
						let _ = tokio::time::timeout(Duration::from_secs(5), &mut reader).await;
						break End::Done;
					};
					let len = text.len() as u64;
					pending.lock().expect("pending").insert(id);
					let sent = sink.send(Message::text(text)).await;
					self.queued.fetch_sub(len, Ordering::Relaxed);
					if let Err(e) = sent {
						reader.abort();
						break End::Dropped(e.to_string());
					}
				},
			}
		};
		let mut ids: Vec<u64> = pending.lock().expect("pending").drain().collect();
		ids.sort_unstable();
		(end, ids)
	}
}

impl Submit for Sender {
	fn submit(&mut self, id: u64, bytes: &[u8]) -> Option<usize> {
		let text = format!(
			r#"{{"jsonrpc":"2.0","id":{id},"method":"author_submitExtrinsic","params":["{}"]}}"#,
			hex0x(bytes)
		);
		for _ in 0..self.connections.len() {
			let i = self.next;
			self.next = (self.next + 1) % self.connections.len();
			let c = &self.connections[i];
			if !c.up.load(Ordering::Relaxed) || c.queued.load(Ordering::Relaxed) > MAX_QUEUED_BYTES
			{
				continue;
			}
			let len = text.len() as u64;
			c.queued.fetch_add(len, Ordering::Relaxed);
			if c.queue.send((id, text.clone())).is_ok() {
				return Some(i);
			}
			c.queued.fetch_sub(len, Ordering::Relaxed); // closed for good: try the next one
		}
		None
	}

	fn queued_bytes(&self) -> u64 {
		self.connections.iter().map(|c| c.queued.load(Ordering::Relaxed)).sum()
	}
}

fn parse_reply(text: &str) -> Option<Reply> {
	let v: serde_json::Value = serde_json::from_str(text).ok()?;
	let id = v["id"].as_u64()?;
	let at = now_ms();
	let Some(e) = v.get("error") else { return Some(Reply::Accepted { id, at }) };
	let message = e["message"].as_str().unwrap_or("");
	let error = match e["data"].as_str() {
		Some(data) => format!("{message}: {data}"),
		None => message.to_owned(),
	};
	Some(Reply::Refused { id, at, code: e["code"].as_i64().unwrap_or(0), error })
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn replies_parse() {
		assert!(matches!(
			parse_reply(r#"{"jsonrpc":"2.0","id":7,"result":"0x12"}"#),
			Some(Reply::Accepted { id: 7, .. })
		));
		let r = parse_reply(
			r#"{"jsonrpc":"2.0","id":8,"error":{"code":1016,"message":"Immediately Dropped","data":"The transaction couldn't enter the pool because of the limit"}}"#,
		);
		let Some(Reply::Refused { id, code, error, .. }) = r else { panic!("refused") };
		assert_eq!((id, code), (8, 1016));
		assert!(error.starts_with("Immediately Dropped: "));
		assert!(parse_reply(r#"{"jsonrpc":"2.0","method":"x","params":{}}"#).is_none());
	}

	/// The id of a request the fake node read.
	fn request_id(msg: &Message) -> u64 {
		let v: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
		v["id"].as_u64().unwrap()
	}

	async fn reply(rx: &mut mpsc::UnboundedReceiver<Reply>) -> Reply {
		tokio::time::timeout(Duration::from_secs(10), rx.recv())
			.await
			.expect("a reply")
			.unwrap()
	}

	/// A proxy or idle timeout closes the socket: the sender opens it again and goes on; the
	/// request it lost is reported as interrupted, and the node is not "down".
	#[tokio::test]
	async fn a_dropped_socket_reconnects_and_is_not_node_down() {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("ws://{}", listener.local_addr().unwrap());
		let node = tokio::spawn(async move {
			// First socket: read one request, then drop it without an answer.
			let (tcp, _) = listener.accept().await.unwrap();
			let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
			let first = ws.next().await.unwrap().unwrap();
			assert_eq!(request_id(&first), 1);
			drop(ws);
			// Second socket: answer everything.
			let (tcp, _) = listener.accept().await.unwrap();
			let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
			while let Some(Ok(msg)) = ws.next().await {
				if msg.is_text() {
					let id = request_id(&msg);
					let answer = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"0x00"}}"#);
					ws.send(Message::text(answer)).await.unwrap();
				}
			}
		});
		let (tx, mut rx) = mpsc::unbounded_channel();
		let backoff = [Duration::from_millis(10); 3];
		let mut sender = Sender::open_with(&url, 1, tx, &backoff).await.unwrap();
		assert_eq!(sender.submit(1, &[1]), Some(0));
		let Reply::Interrupted { connection: 0, ids, .. } = reply(&mut rx).await else {
			panic!("interrupted")
		};
		assert_eq!(ids, [1], "the unanswered request has an unknown outcome");
		assert!(matches!(reply(&mut rx).await, Reply::Reconnected { connection: 0, .. }));
		assert_eq!(sender.submit(2, &[2]), Some(0));
		assert!(matches!(reply(&mut rx).await, Reply::Accepted { id: 2, .. }));
		sender.close();
		node.abort();
	}

	/// A node that stays unreachable after every attempt is down.
	#[tokio::test]
	async fn a_node_that_stays_unreachable_is_down() {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("ws://{}", listener.local_addr().unwrap());
		let node = tokio::spawn(async move {
			let (tcp, _) = listener.accept().await.unwrap();
			let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
			drop(ws);
			drop(listener); // and nothing listens any more
		});
		let (tx, mut rx) = mpsc::unbounded_channel();
		let backoff = [Duration::from_millis(10); 3];
		let mut sender = Sender::open_with(&url, 1, tx, &backoff).await.unwrap();
		assert!(matches!(reply(&mut rx).await, Reply::Interrupted { connection: 0, .. }));
		assert_eq!(reply(&mut rx).await, Reply::Closed { connection: 0, by_node: true });
		assert_eq!(sender.submit(3, &[3]), None, "no connection left to queue on");
		node.await.unwrap();
	}
}
