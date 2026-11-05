//! What the tracker needs from a sender, so the tracker can be tested without a network.

/// Queues one `author_submitExtrinsic`.
pub trait Submit {
	/// Queues request `id` for `bytes`; the connection it went to, or `None` when every
	/// connection is backed up.
	fn submit(&mut self, id: u64, bytes: &[u8]) -> Option<usize>;
	/// Bytes queued and not yet written to the node, over all connections.
	fn queued_bytes(&self) -> u64;
}
