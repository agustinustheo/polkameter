//! Sends txs with `author_submitExtrinsic` over several WebSocket connections, round robin,
//! without waiting for one tx before sending the next. Plain JSON-RPC, so the hot path skips
//! subxt. The sender holds no tx state: the tracker picks request ids and gets every reply back
//! on one channel.
//!
//! A connection counts as backed up with 4 MiB queued and not yet written (as TS
//! `bufferedAmount`), so "pool intake" fires at the same point whatever the tx size.

use std::sync::{
	Arc,
	atomic::{AtomicBool, AtomicU64, Ordering},
};

use futures_util::{SinkExt, StreamExt};
use polkameter_files::Millis;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use polkameter_chain::hex0x;

use crate::{now_ms, submit::Submit};

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
	/// A connection closed (once per connection); its unanswered requests are lost.
	Closed {
		/// Connection index.
		connection: usize,
		/// True when the node closed it, not us.
		by_node: bool,
	},
}

struct Connection {
	queue: mpsc::UnboundedSender<String>,
	queued: Arc<AtomicU64>,
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

impl Sender {
	/// Opens `connections` WebSockets to `url`; replies go to `replies`.
	pub async fn open(
		url: &str,
		connections: usize,
		replies: mpsc::UnboundedSender<Reply>,
	) -> Result<Self, ConnectError> {
		let closing = Arc::new(AtomicBool::new(false));
		let mut out = Vec::with_capacity(connections);
		for connection in 0..connections {
			let (ws, _) = tokio_tungstenite::connect_async(url)
				.await
				.map_err(|source| ConnectError { url: url.to_owned(), source })?;
			let (mut sink, mut stream) = ws.split();
			let (queue, mut rx) = mpsc::unbounded_channel::<String>();
			let queued = Arc::new(AtomicU64::new(0));
			let (writer_queued, reader_replies, closing_r) =
				(queued.clone(), replies.clone(), closing.clone());
			let writer = tokio::spawn(async move {
				while let Some(text) = rx.recv().await {
					let len = text.len() as u64;
					let sent = sink.send(Message::text(text)).await;
					writer_queued.fetch_sub(len, Ordering::Relaxed);
					if sent.is_err() {
						return;
					}
				}
				let _ = sink.close().await; // the queue was dropped: our own close
			});
			tokio::spawn(async move {
				while let Some(msg) = stream.next().await {
					match msg {
						Ok(Message::Text(text)) => {
							if let Some(r) = parse_reply(&text) {
								let _ = reader_replies.send(r);
							}
						},
						Ok(Message::Close(_)) | Err(_) => break,
						Ok(_) => {}, // ping, pong, binary: the node pings every 30 s
					}
				}
				writer.abort();
				let by_node = !closing_r.load(Ordering::Relaxed);
				let _ = reader_replies.send(Reply::Closed { connection, by_node });
			});
			out.push(Connection { queue, queued });
		}
		Ok(Self { connections: out, next: 0, closing })
	}

	/// Closes every connection once its queue is written.
	pub fn close(self) {
		self.closing.store(true, Ordering::Relaxed);
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
			if c.queued.load(Ordering::Relaxed) > MAX_QUEUED_BYTES {
				continue;
			}
			c.queued.fetch_add(text.len() as u64, Ordering::Relaxed);
			if c.queue.send(text).is_ok() {
				return Some(i);
			}
			return None;
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
}
