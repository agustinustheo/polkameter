# Changelog

## Unreleased (0.3.0)

### Changed

- `polkameter-plugin-sdk`: `observers::files::now_ms()` no longer reads the wall clock on every call. It reads it once per process and then advances with tokio's monotonic clock, so it never goes back and ignores wall-clock steps. Timestamps keep their epoch meaning. `wall_ms()` returns the raw wall clock, and `EpochClock` is the clock itself.
- `polkameter-plugin-sdk`: `NodeSample`, `Loss` and `OnChain` are `#[non_exhaustive]`, so new fields can be added without breaking plugins. Build a `NodeSample` with `NodeSample::new`. Start a `Loss` or `OnChain` from `Default` and set its fields. Reading them and deserializing them is unchanged.
- `polkameter-plugin-sdk`: new fields:
  - `NodeSample.pid`: the process sampled.
  - `Loss.state_error`: the plan's state check gave no result.
  - `OnChain.failed_only_on_fork`: dispatch failures only in dropped fork blocks.
  - `Summary.unobserved_ms`: time the block follower was reconnecting.

  Each is left out of the file when empty, and older files read as before.
- Load tests: a dropped RPC connection is opened again, with 15 s of backoff. It is not node down. The stall rules leave out only the time not observed. Unanswered submissions are sent again.
- Load tests: node down is a node unreachable for 15 s, a connection dropping more than 5 times in 60 s, or a node restart.
- Load tests: a transaction's inclusion, latency and `includedPerS` belong to the step that sent it. A block gap counts in each step for the part inside it. A step with no new block counts as slow.
- Load tests: a dispatch failure counts only on the finalized chain when the transaction is there. A failure only on a fork is counted apart.
- Load tests: a state check that errors or times out makes the verdict INCONCLUSIVE instead of PASS. Its sample comes from the finalized chain.
- Load tests: a backed-up connection no longer drops a transaction. Prepared transactions are deduplicated by their decoded hash.
- Load tests: CPU and memory are sampled only for a local node, never for a port forwarder such as an SSH tunnel. A restarted node is followed to its new process.
