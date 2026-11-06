# Changelog

User-visible changes in each release. The GitHub release notes list every merged pull request.

## Unreleased (0.3.0)

Other 0.3.0 changes still need entries here.

### GitHub Action

Moving `@v0` to 0.3.0 changes these defaults for existing workflows:

- **The job fails on a FAIL or INCONCLUSIVE verdict.** The new default `fail-on: verdict-or-inconclusive` fails the job when a load test's verdict is `FAIL` or `INCONCLUSIVE`; before, the default `fail-on: error` failed it only when the run itself failed. A "Why this job failed" section in the job summary, and an error annotation, say what failed the job: the `FAIL` or `INCONCLUSIVE` verdict and what made it, or, for a smoke-mode plan, the monitor problem or missing result that fails it under every `fail-on`. Set `fail-on: verdict` to fail only on `FAIL`, or `fail-on: error` for the old behaviour.
- **Smaller artifact by default.** The run artifact holds only the files PolkaMeter writes: reports, records and each plugin's redacted `stderr.log`. Files that plugins write into the run directory are no longer uploaded. Set `upload-raw-plugin-files: "true"` to upload them.
- **New default artifact name.** The name was `polkameter-run`. It is now `polkameter-run-<job>-<attempt>-<hash>`, unique for each matrix job and each use of the action. Read it from the new `artifact-name` output, or set `artifact-name` to keep a fixed name.
- **Unfinished runs.** If the CLI is killed before the run finishes, plugin logs and summaries are not redacted. The action leaves them out of the artifact and the job summary.
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

- `polkameter run --fail-on-inconclusive` exits `1` when a load test's verdict is `INCONCLUSIVE`. Use it with `--fail-on-verdict` to also fail on `FAIL`. Without either flag the verdict still does not change the exit code. When the report fails the run, a line on standard error says why once the run has ended `completed_with_failures`, for example which required checks have no result. It blames a flag only when the flag decides the exit: a smoke-mode run fails on its problems whatever the flags, and says so. `execution.json` records the same text as `exit_reason`; `--remote` clients print it, and the JUnit `execution` case shows it. A remote agent must advertise the option; older agents are refused before the run starts.
- The CLI spells its name `PolkaMeter` in every message. Standard-error lines now start with `PolkaMeter:` and `PolkaMeter warning:` instead of `Polkameter:`. Update scripts that match the old prefix.

### Load tests

- A dropped RPC connection is opened again, with 15 s of backoff. It is not node down. The stall rules leave out only the time not observed. Unanswered submissions are sent again.
- Node down is a node unreachable for 15 s, a connection dropping more than 5 times in 60 s, or a local node's process restart. A process restart during recovery ends recovery as node down.
- A reconnect that reaches a node with a different peer ID or sync starting block is a warning, not node down: behind a load balancer it can be another healthy node. It is listed under `warnings` in the summary and as a `warning` event, and changes neither the verdict nor the exit code. The stall and finality stall rules still stop the load on a chain that is down.
- The block follower's reads have a 5 s deadline each and stop with the run.
- A transaction's inclusion, latency and `includedPerS` belong to the step that sent it. A block gap counts in each step for the part inside it. A step with no new block counts as slow.
- A dispatch failure counts only on the finalized chain when the transaction is there. A failure only on a fork is counted apart.
- A state check that errors or times out makes the verdict INCONCLUSIVE instead of PASS. Its sample comes from the finalized chain.
- A flood sized exactly to the plan ends on the rate cap (all steps ran), not on budget used up. Budget used up now means the flood ran out before the last step finished, so such a run is no longer INCONCLUSIVE.
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
