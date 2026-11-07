# Troubleshooting

Find the message you see. Each heading is the message as the CLI prints it, without the `PolkaMeter:` prefix. Headings that start with `error:` come from Cargo. A message that already says how to fix it has no entry here.

## Building and installing

<!-- v0.3.0: remove after release -->
Until v0.3.0 is released, `cargo install --locked polkameter` installs v0.2.0, which has no `init` and rejects `--junit`. Install the current code as [Install](install.md#before-v030-is-released) shows.

### error: rustc 1.92.0 is not supported by the following packages:

**Cause:** The default Rust toolchain is older than 1.93, which PolkaMeter needs.

**Fix:** Run `rustup update`. Or install the pinned toolchain with `rustup toolchain install 1.93.0` and build with `cargo +1.93.0 build`.

## Runs

### run failed: … (details in …/execution.json)

```text
Run failed. Artifacts: ~/polkameter/target/runs/run-…
PolkaMeter: run failed: assertion verified failed: expected 85, got 84 (details in ~/polkameter/target/runs/run-…/execution.json)
```

**Cause:** The run started, but a step, a plugin or a deadline failed it. The text after `run failed:` is the reason. Some reasons have their own entry on this page, such as [`assertion <id> failed`](#assertion-id-failed).

**Fix:** Read the full error in `execution.json`. For a failed step, check its `step-finished` event in `events.jsonl`. A failure outside a step, such as the plan's deadline, has no such event.

### run failed before setup: … (details in …/execution.json)

```text
Phase: preflight
Run failed. Artifacts: ~/polkameter/runs/run-…
PolkaMeter: run failed before setup: p.xml: line 2, column 12: plugin example is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry ~/.config/polkameter/plugins.json) (details in ~/polkameter/runs/run-…/execution.json)
```

**Cause:** A check before the setup phase failed, so the run submitted nothing. It can be a credential, a plugin install, a step's inputs, the chain, a `<preflight>` step or the block interval calibration. The run exits `3` and skips teardown.

**Fix:** Find the reason after `run failed before setup:` on this page. Run `polkameter preflight <plan>` to check the fix without a run.

### verdict INCONCLUSIVE: …; exit 1 because of --fail-on-inconclusive

```text
PolkaMeter: verdict INCONCLUSIVE: required checks with no result: no silent loss; exit 1 because of --fail-on-inconclusive (the checks are in summary.md)
```

**Cause:** The run finished, but the verdict is `INCONCLUSIVE`, and `--fail-on-inconclusive` makes that exit `1`. The GitHub Action passes the flag by default. A `verdict FAIL: …` line works the same way with `--fail-on-verdict`. A smoke-mode plan prints `smoke mode fails the run on monitor problems and required checks with no result` instead. Smoke mode exits `1` whatever the flags.

**Fix:** See [What to do about INCONCLUSIVE](reading-results.md#what-to-do-about-inconclusive). To let the job pass meanwhile, drop the flag or set `fail-on: verdict` on the action step. For smoke mode, fix the problem the `smoke:` lines name, or run the plan in stress mode.

### teardown failed: teardown was interrupted by a forced stop

**Cause:** The run was stopped. Then a second signal arrived, or teardown took longer than `--shutdown-grace`. Teardown did not finish, so cleanup on the chain can be incomplete. CI systems often send SIGINT and SIGTERM a few seconds apart.

**Fix:** Raise `--shutdown-grace` to cover your teardown steps. See [Stop a run](cli.md#stop-a-run). Check the chain for state that teardown would have removed.

## Plans

Plan errors start with the plan file and, when known, a line and column: `PolkaMeter: plan.polkameter.xml: line 6, column 5: …`. The headings below write the file as `<plan>`. Most plan messages name the fix, such as an unknown element with a "did you mean" hint.

### \<plan>: line 6, column 5: expected \</setup>, found \</polkameter-plan>; \<setup> was opened at line 3, column 3

**Cause:** The XML is not well formed. Here, `<setup>` is never closed.

**Fix:** Go to the line and column where the element was opened. Close it in the right place.

### The plan file cannot be read

These messages mean PolkaMeter cannot read the file as a plan:

- `plan <plan> is UTF-16 text; save it as UTF-8, the encoding PolkaMeter reads`
- `the plan is not UTF-8 text; save it as UTF-8, the encoding PolkaMeter reads`
- `invalid encoding … in the XML declaration`
- `malformed XML declaration; write <?xml version="1.0" encoding="UTF-8"?>`
- `character U+0000 is not allowed in XML; remove it from the plan`
- `` `--` is not allowed inside a comment, and a comment cannot end with `--->` ``
- `plan <plan> is larger than the 2 MiB (2097152 bytes) limit; split the plan or remove unused content`
- `plan <plan> is empty; if it comes from a pipe or FIFO, check that the command writing it succeeded`
- `cannot read plan <plan>: it is a socket or device, not a plan file; …`

**Cause:** PolkaMeter reads plans as UTF-8. It also accepts a US-ASCII or ISO-8859-1 declaration when the entire plan contains only ASCII characters. Some Windows editors and PowerShell's `>` write UTF-16. A pipe, such as `<(envsubst < plan.tmpl)`, reads as empty when the command that writes it fails.

**Fix:** Save the file as UTF-8 and write `encoding="UTF-8"` in the XML declaration, or remove `encoding`. In PowerShell, use `Set-Content -Encoding utf8`. For an empty pipe, run the command that writes the plan on its own and read its errors. Move large data, such as prepared transactions, into a plugin's output.

### \<plan>: line …: step `<id>` (\<operation>): input `<name>` has value="…", which is …, but \<operation> expects …

```text
PolkaMeter: lit.xml: line 3, column 45: step `a` (core.submit-prepared): input `target` has value="7", which is an integer, but core.submit-prepared expects a string
```

**Cause:** A literal is JSON when it parses as JSON. So `value="7"` is the integer 7, not the text `7`.

**Fix:** Give a value of the expected kind. For text that looks like a number, quote it as JSON: `value="&quot;7&quot;"`.

### assertion \<id> failed

**Cause:** A `core.assert-equal` step found that `actual` and `expected` differ. A failed assertion before setup exits `3`. A failed assertion in setup or later exits `1`.

**Fix:** Correct `expected`, or fix the value that the step computes. The message names the step.

### cannot read … No such file or directory (os error 2)

```text
PolkaMeter: cannot read run directory target/no-such-run: No such file or directory (os error 2)
```

**Cause:** The path after `cannot read` does not exist. The message says what PolkaMeter wanted: a `plan`, a `plugin executable` or a `run directory`.

**Fix:** Check the path. For `report`, pass the `run-…` directory that holds `execution.json`.

## Plugins

Plugin errors point at the plan's `<plugin>` element. `preflight` and `run` exit `3`.

### \<plan>: line …: plugin \<id> is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry …)

**Cause:** The registry in parentheses has no plugin with that ID. `validate` does not check installs.

**Fix:** Run `polkameter plugin install <path-to-executable>` on this host. If the registry path is not the one you installed into, set `POLKAMETER_PLUGIN_REGISTRY` on every step that runs `polkameter`.

### \<plan>: line …: plugin \<id> checksum mismatch: if you rebuilt it, run `polkameter plugin install …` again. …

**Cause:** The executable changed after `plugin install`, usually because you rebuilt it.

**Fix:** Install the plugin again after every rebuild. A checksum or version mismatch stops `run` before setup and exits `3`.

### \<plan>: line …: plugin \<id> version mismatch: set version="\<installed>" in the plan's \<plugin>, or install the build the plan expects …

**Cause:** The plan's `<plugin version="…">` differs from the installed executable's version.

**Fix:** Set the plan's `version` to the installed version. Or install the build that has the plan's version.

### plugin registry \<path> is not a valid registry: …

**Cause:** The registry file is not valid JSON, or has fields PolkaMeter does not know. A manual edit or a full disk can cause this.

**Fix:** Move the damaged registry file to a backup path. Register the plugins and settings again with `plugin install`, `plugin credential`, `plugin topology` and `plugin capability`.

## Credentials and topologies

Credential errors point at the plan's `<credential>` element. `preflight` checks every credential the plan declares. `preflight` and `run` exit `3`. `polkameter plugin list` shows the variable each profile reads, never a value.

### \<plan>: line …: credential `<id>` uses profile `<profile>`, which is not configured on this host; …

```text
PolkaMeter: plan.polkameter.xml: line 2, column 16: credential `sudo` uses profile `sudo`, which is not configured on this host; run `polkameter plugin credential sudo <ENV_VAR>` to name the environment variable that holds it (registry ~/.config/polkameter/plugins.json)
```

**Cause:** The registry maps no environment variable to the profile.

**Fix:** Run the `polkameter plugin credential` command the message shows. With the GitHub Action, add `<profile>=<ENV_VAR>` to its `credentials` input.

### \<plan>: line …: credential `<id>` (profile `<profile>`) reads environment variable \<ENV_VAR> (…), which is not set; …

```text
PolkaMeter: plan.polkameter.xml: line 2, column 16: credential `sudo` (profile `sudo`) reads environment variable SUDO_SEED (mapped by `polkameter plugin credential`), which is not set; set SUDO_SEED on the step that runs polkameter, or map the profile to another variable with `polkameter plugin credential sudo <ENV_VAR>`
```

**Cause:** The variable is not set in the process that runs `polkameter`. The parentheses say where the name came from: the registry, or `--credential-env`. The message says `which is empty` for an empty variable, as an unset CI secret often is. In CI, a variable set on one step is not set on the next.

**Fix:** Set the variable on the step that runs `polkameter`, for example in the step's `env:` block from a secret. Or point the profile, or `--credential-env`, at the variable that holds the secret.

## Chains

### block interval calibration failed during preflight: …

**Cause:** Preflight could not measure the block interval.

**Fix:** Read the cause after the calibration message and check the endpoint. Check the `<target endpoint="ws://…">` value, and that the node is running and reachable.

### measured block interval \<measured>s differs from configured \<configured>s by more than 25%; correct the plan before setup

**Cause:** `block-interval-seconds` on `<load>` differs from the measured block time by more than 25%.

**Fix:** Set `block-interval-seconds` to the measured value. The default is `6.0`.

### Node CPU and memory are not recorded

**Cause:** PolkaMeter samples only a node process on its own machine. A target that is not loopback prints `is not on this machine`. A port that an SSH tunnel serves prints `a port forwarder, not the node`. When the node process ends, a `node down: node: process <pid> ended` warning follows, and the load stops. The load test still works in the first two cases.

**Fix:** For a local node behind another address, set `POLKAMETER_NODE_PID`. To sample a remote node, run PolkaMeter on its machine. After a node down, check the node's logs for why it restarted. See [Node CPU and memory](how-it-works.md#node-cpu-and-memory).

### blocks: RPC connection lost (…); reconnected after \<seconds> s, nothing observed meanwhile

**Cause:** A WebSocket connection to the node dropped, for example at a proxy idle timeout, and PolkaMeter opened it again. The run went on. The `monitors recorded everything` check warns, and smoke mode exits `1`. If blocks went unobserved for longer than the stall limit in all, the check has no result.

**Fix:** If it happens often, check the proxy or load balancer between PolkaMeter and the node. Raise its idle timeout above the run's length.

### reconnected to a different node (load balancer, or the node restarted)

**Cause:** The block follower reconnected, and the node had a new peer ID. Behind a load balancer, the reconnect reached another node. Without one, the node restarted. The verdict and exit code do not change. If the chain stopped producing or finalizing blocks, a stall rule stops the load.

**Fix:** To measure one node, point the run at that node's URL. Without a load balancer, check the node's logs for why it restarted.

### generator limit: the machine was suspended for \<seconds> s

**Cause:** The machine running PolkaMeter slept, or its VM was paused, during the load. The chain went on without load, so the numbers after it are not the chain's.

**Fix:** Keep the machine awake during the run. On a CI runner, check for host maintenance or an overloaded host.

### run failed: insufficient probe reserve

**Cause:** The output that `probes-ref` names holds fewer transactions than the baseline and recovery probes need. With the defaults, the run needs 165.

**Fix:** Pass `ref="run.probeBudget"` as the plugin's probe count, as [Run a load test](load-test.md#write-a-plan) shows. [`<load>`](plans.md#load) gives the formula.

### run failed: duplicate transaction in flood/probe sources

**Cause:** The same transaction appears in both the flood and the probes, or twice in one list.

**Fix:** Use separate prepared transactions for the flood and probes, for example from a signer the flood does not use. After a connection drops, the load sender can resend a transaction whose reply is missing. The ledger still counts it once.

### The node refuses transactions

A node refuses a transaction when its pool will not accept it. The run does not stop on the first refusal. More than 1% refused marks a breaking point. More than 10% in a rate step stops the load.

To see why, open `summary.md` in the run directory. The `refusals are clean` check counts refusals by error code. In `steps.jsonl`, each step's `errors` field counts refusals by code and message.

| Code | Meaning | Usual cause |
| --- | --- | --- |
| `1010` | Invalid transaction. The message says why, such as `Transaction is outdated` or `Inability to pay some fees`. | A stale nonce, a signer without funds, or a transaction built for another runtime version. |
| `1012` | Temporarily banned. | The same transaction was recently refused or dropped. |
| `1013` | Already imported. | The transaction was sent twice. |
| `1014` | Priority is too low. | Another transaction with the same signer and nonce is in the pool. |
| `1016` | Immediately dropped. | The pool is full. |

**Fix:** For `1010` and `1014`, rebuild the transactions against the current chain: fresh nonces, funded signers and the current runtime version. For `1016`, lower the rate or raise the node's pool limits. For other codes, read the node's log at the time of the refusals.

## Remote runs

### remote agent does not support --fail-on-verdict; update the agent

**Cause:** The client asked for `--fail-on-verdict` or `--fail-on-inconclusive`, and the agent does not support it. The message names the flag. The client stops before it starts the run.

**Fix:** Update the agent to the client's PolkaMeter version. Or omit the flag.

### remote runner returned 400 Bad Request: line …: plugin \<id> is not installed on this host; …

**Cause:** The agent's host has no plugin with that ID. The registry in the message is on the agent's host. Clients cannot upload plugins.

**Fix:** Install the plugin on the agent host.

### remote runner returned 400 Bad Request: a plugin plan is already running

**Cause:** The agent runs one plan at a time.

**Fix:** Wait for the run to end, or press Ctrl-C in the client that started it.

### status polls kept failing for 60 s: …

```text
PolkaMeter: status polls kept failing for 60 s: …; asked the agent to stop run <id>
```

**Cause:** The client lost the agent for a minute. The SSH tunnel closed, the agent restarted, or the agent host is overloaded. The client asked the agent to stop the run and exited `1`. A `the start request kept failing for 60 s` message has the same causes.

**Fix:** Restore the tunnel or the agent, then run the plan again. If the message ends with `could not ask the agent to stop run <id>`, wait for that run to end first.

### the run did not end within the client's deadline of … s

**Cause:** The remote run lasted longer than the plan's `timeout-ms`, plus its teardown step timeouts, plus one minute. The client asked the agent to stop it and exited `1`.

**Fix:** Read the run's `events.jsonl` on the agent to find the step that hung. Fix that step or raise its `timeout-ms`.

### remote runner returned 503 Service Unavailable: the agent is shutting down

**Cause:** The agent got SIGINT or SIGTERM and is stopping its active run. It refuses new runs until it exits.

**Fix:** Start the agent again, then run the plan again.

### agent token in POLKAMETER_AGENT_TOKEN must have at least 32 characters, at least 8 of them different; …

**Cause:** The agent's token is too short or repeats a few characters, as in `aaaa…`. Such a token can be guessed.

**Fix:** Set one random token on the agent and the client:

```sh
export POLKAMETER_AGENT_TOKEN=$(openssl rand -hex 32)
```
