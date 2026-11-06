# Changelog

User-visible changes in each release. The GitHub release notes list every merged pull request.

## Unreleased (0.3.0)

Other 0.3.0 changes still need entries here.

### GitHub Action

Moving `@v0` to 0.3.0 changes these defaults for existing workflows:

- **Smaller artifact by default.** The run artifact holds only the files PolkaMeter writes: reports, records and each plugin's redacted `stderr.log`. Files that plugins write into the run directory are no longer uploaded. Set `upload-raw-plugin-files: "true"` to upload them.
- **New default artifact name.** The name was `polkameter-run`. It is now `polkameter-run-<job>-<attempt>-<hash>`, unique for each matrix job and each use of the action. Read it from the new `artifact-name` output, or set `artifact-name` to keep a fixed name.
- **Unfinished runs.** If the CLI is killed before the run finishes, plugin logs and summaries miss PolkaMeter's final redaction pass. The action leaves them out of the artifact and the job summary.
- **Size limit.** At most 500 MiB of run records are uploaded (`artifact-size-limit-mb`). Reports are always uploaded.
- **GitHub Enterprise Server.** The action no longer fails when it uploads the artifact: it skips the upload with a notice. Upload the `artifact-path` output with an uploader your server supports.

Also new:

- `artifact-retention-days` input, and `artifact-name`, `artifact-path` and `artifact-id` outputs.
- Release candidates install from their release: use `@v0.3.0-rc.1` or `version: "0.3.0-rc.1"`. Moving tags never select a prerelease.
- On GHES, if `gh` is not signed in to github.com, `install: auto` builds from source instead of failing; `install: release` fails with guidance.
- Every action the composite action uses is pinned to a commit SHA, so organisations that require SHA-pinned actions can use it.

### Installer

- `install.sh` verifies the build attestation only when `gh` is signed in to github.com. If `gh` is installed but not signed in, as on GitHub-hosted runners, it installs after the checksum check and warns. `POLKAMETER_VERIFY_ATTESTATION=1` still requires verification.
- Failed verification messages mention expired or invalid `gh` credentials.

### Container

- The image runs `polkameter` under `tini`, and `docker stop` sends SIGINT, so a stopped run tears down and writes its results. Allow 45 seconds: `docker stop --timeout 45`.

### Releases

- Releases are published only after every asset is uploaded and attested. Published assets are never replaced.
- crates.io publication waits for the GitHub release and the container image.

### CLI

- The CLI spells its name `PolkaMeter` in every message. Standard-error lines now start with `PolkaMeter:` and `PolkaMeter warning:` instead of `Polkameter:`. Update scripts that match the old prefix.
- Redaction no longer leaves part of a secret visible when one credential contains or overlaps another: all of the text they cover becomes `[redacted]`. It also finds a credential, and a whole endpoint URL, in the escaped forms that JSON writers (including Go's and .NET's HTML-safe ones, and uppercase `\uXXXX`) and Rust's `{:?}` give it. Text that is already `[redacted]` is never matched again, and a credential read with a trailing newline is also redacted without it.
- A plugin's standard error is redacted as it streams into `stderr.log`, so a complete credential never reaches the file, even if the run is killed. If the plugin's output ends in the middle of what may be a credential, that text is written as `[redacted]`. The passes on the first stop request and at the end of the run remain, and read each log in bounded chunks.
- Plugin check results are redacted before `summary.md` is written or printed, so a check detail no longer shows a credential on standard error or in the job summary. `polkameter report` redacts the summary it rewrites with the credentials the host has, and the short `summary.md` written when there is no report, which shows the plan name and the failure reason, is redacted too.
- The load-test records (`steps.jsonl`, `blocks.jsonl`, `scrapes.jsonl`, `lost.jsonl` and the others) are redacted as they are written, so an RPC error, node metric or chain error that echoes a secret no longer shows it. An RPC error is redacted whole before it is cut to its 160-character error key.
- The user information, query values, path parameter values and fragment of target endpoints and monitor URLs are redacted from the run directory, events, standard error and `plugin inspect` and `preflight` output. A monitor URL without a scheme is redacted too. `summary.json`'s `network.url` and `resolved-targets.json`'s `url` keep only the scheme, host, port, path and parameter names.
- JSON object keys are kept unless they are a secret or contain one of 8 or more characters. A credential shorter than 8 characters, such as `//Alice`, gets a warning, because it is redacted wherever it appears.
- Preflight errors now include their cause, as run errors do.

### Load tests

- A dropped RPC connection is opened again, with 15 s of backoff. It is not node down. The stall rules leave out only the time not observed. Unanswered submissions are sent again.
- Node down is a node unreachable for 15 s, a connection dropping more than 5 times in 60 s, or a local node's process restart. A process restart during recovery ends recovery as node down.
- A reconnect that reaches a node with a different peer ID or sync starting block is a warning, not node down: behind a load balancer it can be another healthy node. It is listed under `warnings` in the summary and as a `warning` event, and changes neither the verdict nor the exit code. The stall and finality stall rules still stop the load on a chain that is down.
- The block follower's reads have a 5 s deadline each and stop with the run.
- A transaction's inclusion, latency and `includedPerS` belong to the step that sent it. A block gap counts in each step for the part inside it. A step with no new block counts as slow.
- A dispatch failure counts only on the finalized chain when the transaction is there. A failure only on a fork is counted apart.
- A state check that errors or times out makes the verdict INCONCLUSIVE instead of PASS. Its sample comes from the finalized chain.
- A backed-up connection no longer drops a transaction. Prepared transactions are deduplicated by their decoded hash.
- CPU and memory are sampled only for a local node, never for a port forwarder such as an SSH tunnel. A restarted node is followed to its new process.

### Plugin SDK (`polkameter-plugin-sdk`)

- `observers::files::now_ms()` no longer reads the wall clock on every call. It reads it once per process and then advances with tokio's monotonic clock, so it never goes back and ignores wall-clock steps. Timestamps keep their epoch meaning. `wall_ms()` returns the raw wall clock, and `EpochClock` is the clock itself.
- `NodeSample`, `Loss` and `OnChain` are `#[non_exhaustive]`, so new fields can be added without breaking plugins. Build a `NodeSample` with `NodeSample::new`. Start a `Loss` or `OnChain` from `Default` and set its fields. Reading them and deserializing them is unchanged.
- New fields:
  - `NodeSample.pid`: the process sampled.
  - `Loss.state_error`: the plan's state check gave no result.
  - `OnChain.failed_only_on_fork`: dispatch failures only in dropped fork blocks.
  - `Summary.unobserved_ms`: time the block follower was reconnecting.
  - `Summary.warnings`: what the run shows but does not count as a problem.

  Each is left out of the file when empty, and older files read as before.
- `observers::checks::report::write_redacted` writes the report like `write`, passing every check field, every string in `summary.json` and the whole of `summary.md` through a redaction function before anything is written or printed.
- `observers::files::JsonlWriter::with_filter` passes each record, as JSON, through a function before it is written, for example to redact it.
- `Network::url` holds the redacted URL: PolkaMeter writes it with its user information, query values and fragment as `[redacted]`.
