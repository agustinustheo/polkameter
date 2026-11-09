# Troubleshooting

Find the message you see. Each heading is the start of a message, without the `PolkaMeter:` prefix. `<plan>` stands for your plan file, and `…` for the rest of the text. Headings that start with `error:` come from Cargo.

## Building and installing

### error: rustc 1.92.0 is not supported by the following packages:

The default Rust toolchain is older than 1.93, which PolkaMeter needs.

Run `rustup update`. Or install the pinned toolchain with `rustup toolchain install 1.93.0` and build with `cargo +1.93.0 build`.

## Runs

### run failed: … (details in …/execution.json)

```text
Run failed. Artifacts: ~/polkameter/target/runs/run-…
PolkaMeter: run failed: assertion verified failed: expected 85, got 84 (details in ~/polkameter/target/runs/run-…/execution.json)
```

The run started, but then a step or a plugin failed, or the run hit its deadline. The text after `run failed:` is the reason, and some reasons have their own entry below. `execution.json` keeps the full error. For a failed step, so does its `step-finished` event in `events.jsonl`.

### run failed before setup: … (details in …/execution.json)

```text
Phase: preflight
Run failed. Artifacts: ~/polkameter/runs/run-…
PolkaMeter: run failed before setup: p.xml: line 2, column 12: plugin example is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry ~/.config/polkameter/plugins.json) (details in ~/polkameter/runs/run-…/execution.json)
```

A preflight check failed, so the run submitted nothing. It exits `3` and skips teardown. Look up the reason after `run failed before setup:` on this page. Run `polkameter preflight <plan>` to check the fix without a run.

### verdict INCONCLUSIVE: …; exit 1 …

```text
PolkaMeter: verdict INCONCLUSIVE: required checks with no result: no silent loss; exit 1 ...
```

The run finished, but the verdict is `INCONCLUSIVE`, which exits `1` by default. A `verdict FAIL: …` line works the same way. See [What to do about INCONCLUSIVE](reading-results.md#what-to-do-about-inconclusive). To let the job pass meanwhile, pass `--fail-on verdict`, or set `fail-on: verdict` on the action step.

### teardown failed: teardown was interrupted by a forced stop

You stopped the run, and then a second signal arrived or teardown took longer than [`--shutdown-grace`](cli.md#stop-a-run). Cleanup on the chain can be incomplete, so check it. Raise `--shutdown-grace` to cover your teardown steps.

## Plans

Plan errors start with the plan file and, when known, a line and column. Most name the fix, such as an unknown element with a "did you mean" hint.

### \<plan>: line 6, column 5: expected \</setup>, found \</polkameter-plan>; \<setup> was opened at line 3, column 3

The XML is not well formed. Here, `<setup>` is never closed. Close it in the right place.

### plan \<plan> is empty; if it comes from a pipe or FIFO, check that the command writing it succeeded

A pipe, such as `<(envsubst < plan.tmpl)`, reads as empty when the command that writes it fails. Run that command on its own and read its errors.

### \<plan>: line …: step `<id>` (\<operation>): input `<name>` has value="…", which is …, but \<operation> expects …

```text
PolkaMeter: lit.xml: line 3, column 45: step `a` (core.submit-prepared): input `target` has value="7", which is an integer, but core.submit-prepared expects a string
```

PolkaMeter reads a literal as JSON when it can, so `value="7"` is the integer 7. For text that looks like a number, quote it as JSON: `value="&quot;7&quot;"`.

### assertion \<id> failed

A `core.assert-equal` step found that `actual` and `expected` differ. Correct `expected`, or fix the value that the step computes.

### cannot read … No such file or directory (os error 2)

```text
PolkaMeter: cannot read run directory target/no-such-run: No such file or directory (os error 2)
```

The path after `cannot read` does not exist. For `report`, pass the `run-…` directory that holds `execution.json`.

## Plugins

Plugin errors point at the plan's `<plugin>` element, and `preflight` and `run` exit `3`.

### \<plan>: line …: plugin \<id> is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry …)

If the registry in parentheses is not the one you installed into, set `POLKAMETER_PLUGIN_REGISTRY` on every step that runs `polkameter`.

### \<plan>: line …: plugin \<id> version mismatch: …

The plan's `<plugin version>` differs from the installed plugin's. For the substrate plugin, use the `polkameter --version` number, as [Versions](install.md#versions) says.

### \<plan>: line …: plugin \<id> checksum mismatch: …

The plugin file changed after `plugin install`, usually in an upgrade or a rebuild. Run the `polkameter plugin install` command in the message.

### plugin registry \<path> is not a valid registry: …

The registry file is not valid JSON, after a manual edit or a full disk. Move it to a backup path. Register the plugins and settings again with `plugin install`, `plugin credential` and `plugin topology`.

## Credentials

Credential errors point at the plan's `<credential>` element, and `preflight` and `run` exit `3`. `polkameter plugin list` shows the variable each profile reads, never a value.

### \<plan>: line …: credential `<id>` uses profile `<profile>`, which is not configured on this host; …

This host's registry has no mapping for the profile. Run `polkameter plugin credential <profile> <ENV_VAR>`, then set that variable.

### \<plan>: line …: credential `<id>` (profile `<profile>`) reads environment variable \<ENV_VAR> (…), which is not set; …

```text
PolkaMeter: plan.polkameter.xml: line 2, column 16: credential `sudo` (profile `sudo`) reads environment variable SUDO_SEED (mapped by `polkameter plugin credential`), which is not set; set SUDO_SEED on the step that runs polkameter, or map the profile to another variable with `polkameter plugin credential sudo <ENV_VAR>`
```

The variable is not set in the process that runs `polkameter`. The parentheses say where the name came from: the registry, or [`--credential-env`](cli.md#run). The message says `which is empty` for an empty variable, as an unset CI secret often is. In CI, set it on the step that runs `polkameter`, for example in its `env:` block from a secret.

## Chains

### block interval calibration failed during preflight: …

Preflight could not measure the block interval. Read the cause after the message. Check the `<target endpoint="ws://…">` value, and that the node is running and reachable.

### measured block interval …s differs from configured …s by more than 25%; …

Set the load's `block-interval-seconds` to the measured value, or let [`polkameter init`](cli.md#init) write the plan.

### node: … no CPU or memory recorded

PolkaMeter reads a node's CPU and memory from the operating system, so it can only sample a node on the same machine. It samples the process in `POLKAMETER_NODE_PID`, or the process listening on the RPC port of a local URL (`localhost`, `127.0.0.1` or `::1`).

If the node restarts, PolkaMeter samples the new process on that port. A restart during the load stops the load on the node down rule. With `POLKAMETER_NODE_PID` and a URL that isn't local, PolkaMeter can't find a restarted node again.

### blocks: RPC connection lost (…); reconnected after \<seconds> s, nothing observed meanwhile

A connection to the node dropped, often at a proxy idle timeout, and PolkaMeter reconnected within 15 s. If it happens often, raise the proxy's idle timeout above the run's length.

PolkaMeter uses one WebSocket connection to follow blocks and several to send. While the block connection is down, PolkaMeter can't see blocks. The stall rules don't count that time. They count every other second since the last new block. If blocks went unseen for longer than the stall limit in total, the "monitors recorded everything" check has no result, and the verdict can't be PASS.

After a sending connection drops, PolkaMeter can resend a transaction whose reply is missing. Loss accounting still counts it once. [Stop rules](reading-results.md#stop-rules) says when the node counts as down.

### generator limit: the machine was suspended for \<seconds> s; the numbers after this are not the chain's

The machine running PolkaMeter, the load generator, slept or was paused during the load. The chain kept going without load, so the numbers after the pause don't measure the chain. Keep the machine awake. On a CI runner, check for host maintenance or an overloaded host.

### run failed: insufficient probe reserve

The output that `probes-ref` names holds fewer transactions than the probes need. Pass `ref="run.probeBudget"` as the plugin's probe count. [`<load>`](plans.md#load) gives the formula.

### run failed: duplicate transaction in flood/probe sources

The same transaction appears in both the flood and the probes, or twice in one list. Prepare the probes from signers the flood doesn't use.

### The node refuses transactions

This entry is a symptom, not a message. A node keeps transactions in its pool until a block includes them. It refuses a transaction when the pool won't take it. More than 1% refused in a rate step marks the failure onset. More than 10% stops the load.

To see why, open `summary.md` in the run directory. The `pool: refusals are clean` check counts refusals by error code. In `steps.jsonl`, each rate step's `errors` field counts refusals by code and message.

| Code | Meaning | Usual cause |
| --- | --- | --- |
| `1010` | Invalid transaction. The message says why, such as `Transaction is outdated` or `Inability to pay some fees`. | An already used [nonce](substrate-plugin.md#signers-and-secrets), a signer without funds, or a transaction built for another runtime version. |
| `1012` | Temporarily banned. | The same transaction was recently refused or dropped. |
| `1013` | Already imported. | The transaction was sent twice. |
| `1014` | Priority is too low. | Another transaction with the same signer and nonce is in the pool. |
| `1016` | Immediately dropped. | The pool is full. |

For `1010` and `1014`, rebuild the transactions against the current chain: fresh nonces, funded signers and the current runtime version. For `1016`, lower the rate or raise the node's pool limits. For other codes, read the node's log at the time of the refusals.

## Substrate plugin

### input mortality …: the transactions stay valid for about … s …, less than expected-seconds …

`mortality` is how many blocks a signed transaction stays valid. The prepare step's transactions would expire before the run sends them. Raise `mortality` to the value the message suggests, or set it to `0` so they never expire.

### … signs with …, which an earlier prepare step of this run also signs with; …

Two prepare steps share signers, so they would sign different transactions with the same nonces. Only one of each pair can run. Give the second step another `derivation`, and fund those signers with their own `fund-accounts` step.
