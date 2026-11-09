//! What the tracker needs from a sender, so the tracker can be tested without a network.

/// Queues one `author_submitExtrinsic`.
pub trait Submit {
	/// Queues request `id` for `bytes`; the connection it went to, or `None` when every open
	/// connection is backed up or none is open.
	fn submit(&mut self, id: u64, bytes: &[u8]) -> Option<usize>;
	/// Bytes queued and not yet written to the node, over all connections.
	fn queued_bytes(&self) -> u64;
	/// No connection is open: every one dropped and is being opened again. A submit that is not
	/// queued then is not the node refusing to read.
	fn disconnected(&self) -> bool {
		false
	}
}
