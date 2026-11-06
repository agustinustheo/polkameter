# Changelog

User-visible changes in each release. The GitHub release notes list every merged pull request.

## Unreleased (0.3.0)

Other 0.3.0 changes still need entries here.

### Substrate plugin and dev chain

- New: the substrate plugin, `polkameter-substrate-plugin`, loads any chain with the standard `System` and `Balances` pallets without plugin code. It funds signers derived from one seed, signs remark or transfer floods and probes, and checks the state after a run. The release archives, the install script, the container and the action include it; it is not published on crates.io.
- The substrate plugin's `fund-accounts` checks before it sends anything that the funder can spend the top-ups, the fees and its existential deposit, and after the transfers are final that every signer can spend `amount`; it fails, naming the signers and the dispatch error, instead of reporting success. It first waits for the funder's and the signers' transactions still in the pool, so a retry never tops up twice. Balances are spendable (free minus frozen) everywhere.
- The substrate plugin gives each probe its own signer by default (pass `probes` to `fund-accounts` too), refuses a second prepare step that signs with the same accounts, checks `mortality` against the new `expected-seconds` input, signs periods above 4096 blocks against the era's first block, and checks in `preflight` that the runtime's address and signature types take sr25519 keys and that the chain accepts its transaction format.
- New: `polkameter init --rpc <endpoint>` writes a load plan for the substrate plugin from a live chain. It sets the mortality from the measured block time and the plan's length, names the credential profile `substrate-dev`, and creates the `--output` directory.
- New: `scripts/dev-chain.sh start|stop` runs a pinned, checksummed `polkadot-omni-node` dev chain with the parachain template runtime, or your own runtime, for local and CI load tests. It keeps its files in `.polkameter/dev-chain` and signals only the node it started.
- New: an Agent Skill in `skills/polkameter`, `AGENTS.md` and `docs/llms.txt` help AI coding assistants set PolkaMeter up. The repository is a Claude Code plugin marketplace: `/plugin marketplace add agustinustheo/polkameter`, then `/plugin install polkameter@polkameter`.
- The GitHub Action's `plugins` input accepts a bare executable name, such as `polkameter-substrate-plugin`, and finds it on `PATH`.

### GitHub Action

Moving `@v0` to 0.3.0 changes these defaults for existing workflows:

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

- The CLI spells its name `PolkaMeter` in every message. Standard-error lines now start with `PolkaMeter:` and `PolkaMeter warning:` instead of `Polkameter:`. Update scripts that match the old prefix.

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

- New in the `chain` feature: `TxBuilder` signs calls for any runtime from its metadata, with an explicit nonce, mortality and tip, and `sign_batch` signs many with consecutive nonces. `keypair`, `derive_range` and `parse_account` make test signers from secret URIs. `Client::account`, `Client::existential_deposit`, `Client::finalized_head` and `Client::tx_builder` read what signing needs. `TxBuilder::mortal` reads the hash of the era's first block, which for periods above 4096 is before the builder's block (`era_birth`), and `sign` refuses a mortality that starts elsewhere. `TxBuilder::check_signing_types` tells whether sr25519 keys can sign for the runtime. `Client::query_fee`, `Client::spendable`, `AccountInfo::spendable` and `Client::recent_block_seconds` are new.
- `observers::files::now_ms()` no longer reads the wall clock on every call. It reads it once per process and then advances with tokio's monotonic clock, so it never goes back and ignores wall-clock steps. Timestamps keep their epoch meaning. `wall_ms()` returns the raw wall clock, and `EpochClock` is the clock itself.
- `NodeSample`, `Loss` and `OnChain` are `#[non_exhaustive]`, so new fields can be added without breaking plugins. Build a `NodeSample` with `NodeSample::new`. Start a `Loss` or `OnChain` from `Default` and set its fields. Reading them and deserializing them is unchanged.
- New fields:
  - `NodeSample.pid`: the process sampled.
  - `Loss.state_error`: the plan's state check gave no result.
  - `OnChain.failed_only_on_fork`: dispatch failures only in dropped fork blocks.
  - `Summary.unobserved_ms`: time the block follower was reconnecting.
  - `Summary.warnings`: what the run shows but does not count as a problem.

  Each is left out of the file when empty, and older files read as before.
