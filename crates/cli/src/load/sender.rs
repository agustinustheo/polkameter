//! Sends txs with `author_submitExtrinsic` over several WebSocket connections, round robin,
//! without waiting for one tx before sending the next. Plain JSON-RPC, so the hot path skips
//! subxt. The sender holds no tx state: the tracker picks request ids and gets every reply back
//! on one channel.
//!
//! A connection counts as backed up with 4 MiB queued and not yet written (as TS
//! `bufferedAmount`), so "pool intake" fires at the same point whatever the tx size.
//!
//! A connection the node or the network closes is opened again, with backoff (see
//! [`crate::load::reconnect`]). The requests it had written and not seen answered are reported
//! as [`Reply::Interrupted`] and sent again, first, on the new socket with the same ids: the node
//! may never have read them, and a later nonce would wait behind a missing one. A node that had
//! read one answers "already imported" or "temporarily banned", which the tracker takes as
//! accepted. Requests still queued are written after them. While a connection is down, submits
//! go to the others. When the node stays unreachable for the backoff, or the connection keeps
//! dropping, it ends with [`Reply::Closed`], which the "node down" rule reads.

use std::{
	collections::BTreeMap,
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

use tokio_util::sync::CancellationToken;

use crate::load::{
	now_ms,
	reconnect::{Backoff, Drops, Reconnect, reconnect},
	submit::Submit,
};

const MAX_QUEUED_BYTES: u64 = 4 * 1024 * 1024;

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
	/// answered are sent again on the new socket. Not a node failure.
	Interrupted {
		/// Connection index.
		connection: usize,
		/// The unanswered request ids, to be sent again.
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
		/// True when it could not be opened again, or kept dropping: the node is down. False
		/// when we closed it.
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
	closing: CancellationToken,
}

/// A connection could not be opened: a wrong URL or a node that is down before the run.
#[derive(Debug, thiserror::Error)]
#[error("sender: cannot connect to {url}: {why}")]
pub struct ConnectError {
	url: String,
	why: String,
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Opens a WebSocket within `limit`: a server that accepts TCP and never answers the handshake
/// fails the attempt instead of holding it.
async fn connect(url: &str, limit: Duration) -> Result<Socket, String> {
	match tokio::time::timeout(limit, tokio_tungstenite::connect_async(url)).await {
		Ok(Ok((ws, _))) => Ok(ws),
		Ok(Err(e)) => Err(e.to_string()),
		Err(_) => Err(format!("no WebSocket handshake within {} s", limit.as_secs())),
	}
}

impl Sender {
	/// Opens `connections` WebSockets to `url`; replies go to `replies`.
	pub async fn open(
		url: &str,
		connections: usize,
		replies: mpsc::UnboundedSender<Reply>,
	) -> Result<Self, ConnectError> {
		Self::open_with(url, connections, replies, &Backoff::default()).await
	}

	/// [`Sender::open`], with the backoff of reconnects.
	pub async fn open_with(
		url: &str,
		connections: usize,
		replies: mpsc::UnboundedSender<Reply>,
		backoff: &Backoff,
	) -> Result<Self, ConnectError> {
		let closing = CancellationToken::new();
		let mut out = Vec::with_capacity(connections);
		for connection in 0..connections {
			let ws = connect(url, backoff.attempt)
				.await
				.map_err(|why| ConnectError { url: crate::redact::endpoint(url), why })?;

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
				backoff: backoff.clone(),
			};
			tokio::spawn(link.run(ws, rx));
			out.push(Connection { queue, queued, up });
		}
		Ok(Self { connections: out, next: 0, closing })
	}

	/// Closes every connection once its queue is written.
	pub fn close(self) {
		self.closing.cancel();
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
	closing: CancellationToken,
	backoff: Backoff,
}

/// How a socket's session ended.
enum End {
	/// Our queue was dropped and written out: our own close.
	Done,
	/// The socket dropped.
	Dropped(String),
}

/// Requests written and not answered yet, by id: sent again on the next socket.
type Unanswered = Arc<Mutex<BTreeMap<u64, String>>>;

impl Link {
	async fn run(self, mut ws: Socket, mut rx: mpsc::UnboundedReceiver<(u64, String)>) {
		let unanswered = Unanswered::default();
		let mut drops = Drops::default();
		loop {
			let reason = match self.session(ws, &mut rx, &unanswered).await {
				End::Done => return self.closed(false),
				End::Dropped(_) if self.closing.is_cancelled() => return self.closed(false),
				End::Dropped(reason) => reason,
			};
			// Down before the tracker hears of it: a submit it makes after `Interrupted` must not
			// land on this connection and wait on the node through the outage.
			self.up.store(false, Ordering::Relaxed);
			let ids: Vec<u64> = unanswered.lock().expect("unanswered").keys().copied().collect();
			let _ = self.replies.send(Reply::Interrupted {
				connection: self.connection,
				ids,
				reason: reason.clone(),
			});
			if !drops.record() {
				eprintln!("sender: connection {}: {}", self.connection, Drops::reason());
				return self.closed(true);
			}
			let down_at = now_ms();
			let attempt = self.backoff.attempt;
			match reconnect(|| connect(&self.url, attempt), &self.backoff, &self.closing).await {
				Reconnect::Connected(socket) => {
					ws = socket;
					self.up.store(true, Ordering::Relaxed);
					let _ = self.replies.send(Reply::Reconnected {
						connection: self.connection,
						down_ms: now_ms().saturating_sub(down_at),
					});
				},
				Reconnect::Stopped => return self.closed(false),
				Reconnect::GaveUp(why) => {
					eprintln!(
						"sender: connection {} dropped ({reason}) and the node was {why}",
						self.connection
					);
					return self.closed(true);
				},
			}
		}
	}

	fn closed(&self, by_node: bool) {
		let _ = self.replies.send(Reply::Closed { connection: self.connection, by_node });
	}

	/// Sends what the last socket left unanswered again, then writes the queue and reads replies
	/// until the socket drops or the queue is done.
	async fn session(
		&self,
		ws: Socket,
		rx: &mut mpsc::UnboundedReceiver<(u64, String)>,
		unanswered: &Unanswered,
	) -> End {
		let (mut sink, mut stream) = ws.split();
		let (reader_unanswered, replies) = (unanswered.clone(), self.replies.clone());
		// Reading runs apart from writing, so a slow write never stops replies being read.
		let mut reader = tokio::spawn(async move {
			while let Some(msg) = stream.next().await {
				match msg {
					Ok(Message::Text(text)) => {
						if let Some(r) = parse_reply(&text) {
							if let Reply::Accepted { id, .. } | Reply::Refused { id, .. } = &r {
								reader_unanswered.lock().expect("unanswered").remove(id);
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
		// The node may never have read these: send them again, in order (same ids).
		let resend: Vec<String> =
			unanswered.lock().expect("unanswered").values().cloned().collect();
		for text in resend {
			if let Err(e) = sink.send(Message::text(text)).await {
				reader.abort();
				return End::Dropped(e.to_string());
			}
		}
		loop {
			tokio::select! {
				reason = &mut reader => return End::Dropped(reason.unwrap_or_else(|e| e.to_string())),
				item = rx.recv() => {
					let Some((id, text)) = item else {
						let _ = sink.close().await; // the queue was dropped: our own close
						// Replies to what was written still arrive until the node closes.
						let _ = tokio::time::timeout(Duration::from_secs(5), &mut reader).await;
						return End::Done;
					};
					let len = text.len() as u64;
					unanswered.lock().expect("unanswered").insert(id, text.clone());
					let sent = sink.send(Message::text(text)).await;
					self.queued.fetch_sub(len, Ordering::Relaxed);
					if let Err(e) = sent {
						reader.abort();
						return End::Dropped(e.to_string());
					}
				},
			}
		}
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

	fn backoff() -> Backoff {
		Backoff {
			waits: vec![Duration::from_millis(10)],
			attempt: Duration::from_millis(500),
			total: Duration::from_secs(2),
		}
	}

	/// Answers every request on `ws` with a success.
	async fn answer_all(mut ws: WebSocketStream<TcpStream>, seen: mpsc::UnboundedSender<u64>) {
		while let Some(Ok(msg)) = ws.next().await {
			if msg.is_text() {
				let id = request_id(&msg);
				let _ = seen.send(id);
				let answer = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"0x00"}}"#);
				ws.send(Message::text(answer)).await.unwrap();
			}
		}
	}

	/// A proxy or idle timeout closes the socket after the node read nothing of a request: the
	/// sender opens it again, sends the unanswered request again first, and the node is not
	/// "down". (A dropped submit would leave every later nonce stuck in the future pool.)
	#[tokio::test]
	async fn a_dropped_socket_reconnects_and_resends_the_unanswered() {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("ws://{}", listener.local_addr().unwrap());
		let (seen_tx, mut seen) = mpsc::unbounded_channel();
		let node = tokio::spawn(async move {
			// First socket: read one request, then drop it without an answer.
			let (tcp, _) = listener.accept().await.unwrap();
			let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
			let first = ws.next().await.unwrap().unwrap();
			assert_eq!(request_id(&first), 1);
			drop(ws);
			// Second socket: answer everything.
			let (tcp, _) = listener.accept().await.unwrap();
			answer_all(tokio_tungstenite::accept_async(tcp).await.unwrap(), seen_tx).await;
		});
		let (tx, mut rx) = mpsc::unbounded_channel();
		let mut sender = Sender::open_with(&url, 1, tx, &backoff()).await.unwrap();
		assert_eq!(sender.submit(1, &[1]), Some(0));
		let Reply::Interrupted { connection: 0, ids, .. } = reply(&mut rx).await else {
			panic!("interrupted")
		};
		assert_eq!(ids, [1], "the unanswered request is sent again");
		assert!(matches!(reply(&mut rx).await, Reply::Reconnected { connection: 0, .. }));
		assert!(matches!(reply(&mut rx).await, Reply::Accepted { id: 1, .. }), "resent");
		assert_eq!(sender.submit(2, &[2]), Some(0));
		assert!(matches!(reply(&mut rx).await, Reply::Accepted { id: 2, .. }));
		assert_eq!((seen.recv().await, seen.recv().await), (Some(1), Some(2)), "in order");
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
		let mut sender = Sender::open_with(&url, 1, tx, &backoff()).await.unwrap();
		assert!(matches!(reply(&mut rx).await, Reply::Interrupted { connection: 0, .. }));
		assert_eq!(reply(&mut rx).await, Reply::Closed { connection: 0, by_node: true });
		assert_eq!(sender.submit(3, &[3]), None, "no connection left to queue on");
		node.await.unwrap();
	}

	/// A node that accepts TCP and never answers the WebSocket handshake: each attempt times out
	/// and the node is down when the backoff's deadline passes, instead of the sender hanging.
	#[tokio::test]
	async fn a_stalled_handshake_ends_in_node_down_at_the_deadline() {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("ws://{}", listener.local_addr().unwrap());
		let node = tokio::spawn(async move {
			let (tcp, _) = listener.accept().await.unwrap();
			let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
			drop(ws);
			let mut held = Vec::new();
			loop {
				let (tcp, _) = listener.accept().await.unwrap();
				held.push(tcp); // never a handshake
			}
		});
		let (tx, mut rx) = mpsc::unbounded_channel();
		let _sender = Sender::open_with(&url, 1, tx, &backoff()).await.unwrap();
		assert!(matches!(reply(&mut rx).await, Reply::Interrupted { connection: 0, .. }));
		let started = std::time::Instant::now();
		assert_eq!(reply(&mut rx).await, Reply::Closed { connection: 0, by_node: true });
		assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
		node.abort();
	}
}
