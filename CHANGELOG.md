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
- **Breaking: `run` exits `3` when it fails before its setup phase.** A credential that is not mapped, unset or empty, a plugin that is not installed, a checksum, version or manifest mismatch, a step input that does not match its operation, missing chain evidence, a failed `<preflight>` step or a calibration failure used to exit `1`, like a failed test. They now exit `3`, as `polkameter preflight` does, and the reason line reads `run failed before setup:`. Nothing was submitted, so teardown is skipped. `execution.json` keeps `"state": "failed"` with `"exit_code": 3`, and gains `failed_phase` and, for errors about one element, `error_location`. CI scripts that compare the exit code with `1` must also accept `3`; the GitHub Action fails the job on any nonzero code, as before.
- **Breaking: `plugin list` prints the whole registry view.** It was the installed plugins alone. It is now an object with `plugins`, `credentials` (each profile's environment variable name, never a value), `topologies` and `capabilities`. Read `.plugins` for the old output.
- **Credential errors name the variable and how to set it.** `credential profile sudo is unavailable` is now ``credential `sudo` (profile `sudo`) reads environment variable SUDO_SEED (mapped by `polkameter plugin credential`), which is not set; set SUDO_SEED on the step that runs polkameter, …``, with the plan position. `preflight` checks every credential the plan declares, not only those a preflight step uses, and never passes the values anywhere.
- **Plans from pipes.** `polkameter validate <(envsubst < plan.tmpl)` and `cat plan | polkameter validate /dev/stdin` work again. A FIFO that nothing writes to reads as an empty plan instead of hanging. Directories, sockets and devices are still refused.
- **`validate` checks built-in operations.** A misspelt `core.` operation, a wrong or missing input of one, a literal of the wrong type, or a reference to an output a core step does not have now fails `validate` instead of the run. The unknown-operation message lists the core operations.
- **Plan parsing is linear.** A plan with tens of thousands of elements used to take minutes to parse, because every position was counted from the start of the file. The remote agent parses client plans the same way.
- **Clearer XML and attribute messages.** A mismatched end tag says where the element was opened. An unknown attribute or element suggests the closest name and lists the allowed ones. A number that does not parse says what kind of number is expected and shows the value. An unterminated `&` suggests `&amp;`. A UTF-16 file is named as such. An XML declaration of `US-ASCII` or `ISO-8859-1` is accepted while the plan is plain ASCII.
- **Run errors point into the plan.** A run that fails on one element, such as a step input, prints the plan file, line and column, and its JSON `artifact-written` line has `file`, `line` and `column`.
- **Plugin errors put the fix first.** Checksum and version mismatch messages start with the command to run, so the fix survives the 300-character reason line; long hashes are shortened to 12 characters on that line. `plugin … is not installed` names the plan's `<plugin>` element and the registry file it read.
- **Registry writes are safer.** The temporary file is created exclusively (never through a planted link) and owner-only, and takes the registry's permissions. A registry path that is a symbolic link is written through to its target. Temporary files that a crashed writer left are removed after an hour. A command that waits for another one's lock says so on standard error.
- **Output errors are reported.** A failed write to standard output other than a closed pipe, such as a full disk, prints `PolkaMeter: cannot write to standard output: …` and makes a command that would exit `0` exit `1`. A closed standard error no longer panics during a load run's summary or `report`. A path argument that is not UTF-8 no longer panics in `--format json`; `run --output` refuses one with exit code 2.
- The CLI spells its name `PolkaMeter` in every message. Standard-error lines now start with `PolkaMeter:` and `PolkaMeter warning:` instead of `Polkameter:`. Update scripts that match the old prefix.
- **Plan errors name the file and position.** Every plan error starts with the plan file and, when it is about one element, the line and column: `PolkaMeter: plan.xml: line 4, column 46: …`. Errors about a step name the step's ID, its operation and the input. Most validation and preflight messages were reworded to say what is wrong and how to fix it; for example `unknown or forward reference steps.x.y` now reads ``step `a` (core.echo): input `value` refers to `steps.x.y`, but step `x` does not run before this step; move `x` earlier …``. Update scripts that match the old message text.
- **Plugin mismatches say which and how to fix.** `plugin <id> version/checksum mismatch` is now `plugin <id> version mismatch: set version="…" in the plan's <plugin>, or install …` or `plugin <id> checksum mismatch: if you rebuilt it, run `polkameter plugin install <path>` again. … (BLAKE2 expected …, found …)`. An unknown operation lists the operations the plugin offers, and an unknown input lists the operation's inputs.
- **Stricter XML.** Plans are rejected, with a line and column, when a comment contains `--`, when the XML declaration is malformed or names an encoding other than UTF-8, when the declaration is not at the very start of the file, or when the file contains a character XML 1.0 does not allow, such as NUL.
- **Bounded plan reading.** PolkaMeter reads at most 2 MiB plus one byte of a plan and refuses a larger one, naming the limit. A directory, socket or device is refused at once instead of hanging the command. A file that is not UTF-8 is refused with the position of the first invalid byte, also as `line` and `column` in JSON.
- **Concurrent `plugin` commands are safe.** `plugin install`, `credential`, `topology` and `capability` lock `<registry>.lock`, reread the registry and replace it atomically, so parallel commands no longer lose each other's changes. A corrupt registry names its path and how to recover, and is never overwritten.
- **A closed standard output no longer panics.** `polkameter … | head -1` used to end with a panic and exit code 101. Output now stops quietly and the command finishes with its own exit code.
- **More help.** Every subcommand, argument and flag has a description, and `--help` shows examples and the exit codes.

### CLI JSON output (breaking for scripts that parse `--format json`)

- Every line now has `event` and `schemaVersion` (`1`). The `preflight` result gains `"event":"preflight"`.
- A command that fails with `--format json` now prints an `error` line on standard output, with `command`, `kind`, `exitCode` and `message`, and for a plan error `file`, `line` and `column`. A failed `validate` also has `"valid":false`. Before, it printed nothing on standard output. Scripts that treated any standard output as success must check `event` instead.
- Keys that the CLI adds are camelCase, like `summary.json`. The `outcome` object keeps the keys of `execution.json` (`artifact_dir`, `exit_code`), and plugin manifests keep the plugin protocol's keys (`read_only`). No existing key was renamed.
- [Output formats](docs/cli.md#output-formats) documents the contract.

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
