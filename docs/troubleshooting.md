# Troubleshooting

Find the message you see. Each heading is the message as the CLI prints it, without the `PolkaMeter:` prefix. Headings that start with `error:` come from Cargo. A message that already says how to fix it has no entry here.

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

The run started, but a step, a plugin or a deadline failed it. The text after `run failed:` is the reason. Some reasons have their own entry on this page, such as [`assertion <id> failed`](#assertion-id-failed).

Read the full error in `execution.json`. For a failed step, check its `step-finished` event in `events.jsonl`. A failure outside a step, such as the plan's deadline, has no such event.

### run failed before setup: … (details in …/execution.json)

```text
Phase: preflight
Run failed. Artifacts: ~/polkameter/runs/run-…
PolkaMeter: run failed before setup: p.xml: line 2, column 12: plugin example is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry ~/.config/polkameter/plugins.json) (details in ~/polkameter/runs/run-…/execution.json)
```

A check before the setup phase failed, so the run submitted nothing. It can be a credential, a plugin install, a step's inputs, the chain, a `<preflight>` step or the block interval calibration. The run exits `3` and skips teardown.

Find the reason after `run failed before setup:` on this page. Run `polkameter preflight <plan>` to check the fix without a run.

### verdict INCONCLUSIVE: …; exit 1 because of --fail-on-inconclusive

```text
PolkaMeter: verdict INCONCLUSIVE: required checks with no result: no silent loss; exit 1 because of --fail-on-inconclusive (the checks are in summary.md)
```

The run finished, but the verdict is `INCONCLUSIVE`, and `--fail-on-inconclusive` makes that exit `1`. The GitHub Action passes the flag by default. A `verdict FAIL: …` line works the same way with `--fail-on-verdict`. A smoke-mode monitor problem or missing required check result exits `1` regardless of the flags. Other verdict failures still name the flag.

See [What to do about INCONCLUSIVE](reading-results.md#what-to-do-about-inconclusive). To let the job pass meanwhile, drop the flag or set `fail-on: verdict` on the action step. For smoke mode, fix the problem the `smoke:` lines name, or run the plan in stress mode.

### teardown failed: teardown was interrupted by a forced stop

The run was stopped. Then a second signal arrived, or teardown took longer than `--shutdown-grace`. Teardown did not finish, so cleanup on the chain can be incomplete. CI systems often send SIGINT and SIGTERM a few seconds apart.

Raise `--shutdown-grace` to cover your teardown steps. See [Stop a run](cli.md#stop-a-run). Check the chain for state that teardown would have removed.

## Plans

Plan errors start with the plan file and, when known, a line and column: `PolkaMeter: plan.polkameter.xml: line 6, column 5: …`. The headings below write the file as `<plan>`. Most plan messages name the fix, such as an unknown element with a "did you mean" hint.

### \<plan>: line 6, column 5: expected \</setup>, found \</polkameter-plan>; \<setup> was opened at line 3, column 3

The XML is not well formed. Here, `<setup>` is never closed.

Go to the line and column where the element was opened. Close it in the right place.

### plan \<plan> is UTF-16 text; save it as UTF-8, the encoding PolkaMeter reads

Some Windows editors and PowerShell's `>` write UTF-16. In PowerShell, use `Set-Content -Encoding utf8`.

### plan \<plan> is empty; if it comes from a pipe or FIFO, check that the command writing it succeeded

A pipe, such as `<(envsubst < plan.tmpl)`, reads as empty when the command that writes it fails. Run that command on its own and read its errors.

### \<plan>: line …: step `<id>` (\<operation>): input `<name>` has value="…", which is …, but \<operation> expects …

```text
PolkaMeter: lit.xml: line 3, column 45: step `a` (core.submit-prepared): input `target` has value="7", which is an integer, but core.submit-prepared expects a string
```

A literal is JSON when it parses as JSON. So `value="7"` is the integer 7, not the text `7`.

Give a value of the expected kind. For text that looks like a number, quote it as JSON: `value="&quot;7&quot;"`.

### assertion \<id> failed

A `core.assert-equal` step found that `actual` and `expected` differ. A failed assertion before setup exits `3`. A failed assertion in setup or later exits `1`.

Correct `expected`, or fix the value that the step computes. The message names the step.

### cannot read … No such file or directory (os error 2)

```text
PolkaMeter: cannot read run directory target/no-such-run: No such file or directory (os error 2)
```

The path after `cannot read` does not exist. The message says what PolkaMeter wanted: a `plan`, a `plugin executable` or a `run directory`.

Check the path. For `report`, pass the `run-…` directory that holds `execution.json`.

## Plugins

Plugin errors point at the plan's `<plugin>` element. `preflight` and `run` exit `3`.

### \<plan>: line …: plugin \<id> is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry …)

The registry in parentheses has no plugin with that ID. `validate` does not check installs.

Run `polkameter plugin install <path-to-executable>` on this host. If the registry path is not the one you installed into, set `POLKAMETER_PLUGIN_REGISTRY` on every step that runs `polkameter`.

### \<plan>: line …: plugin \<id> checksum mismatch: if you rebuilt it, run `polkameter plugin install …` again. …

The executable changed after `plugin install`, usually because you rebuilt it.

Install the plugin again after every rebuild. A checksum or version mismatch stops `run` before setup and exits `3`.

### plugin registry \<path> is not a valid registry: …

The registry file is not valid JSON. A manual edit or a full disk can cause this.

Move the damaged registry file to a backup path. Register the plugins and settings again with `plugin install`, `plugin credential` and `plugin topology`.

## Credentials and topologies

Credential errors point at the plan's `<credential>` element. `preflight` checks every credential the plan declares. `preflight` and `run` exit `3`. `polkameter plugin list` shows the variable each profile reads, never a value.

### \<plan>: line …: credential `<id>` (profile `<profile>`) reads environment variable \<ENV_VAR> (…), which is not set; …

```text
PolkaMeter: plan.polkameter.xml: line 2, column 16: credential `sudo` (profile `sudo`) reads environment variable SUDO_SEED (mapped by `polkameter plugin credential`), which is not set; set SUDO_SEED on the step that runs polkameter, or map the profile to another variable with `polkameter plugin credential sudo <ENV_VAR>`
```

The variable is not set in the process that runs `polkameter`. The parentheses say where the name came from: the registry, or `--credential-env`. The message says `which is empty` for an empty variable, as an unset CI secret often is. In CI, a variable set on one step is not set on the next.

Set the variable on the step that runs `polkameter`, for example in the step's `env:` block from a secret. Or point the profile, or `--credential-env`, at the variable that holds the secret.

## Chains

### block interval calibration failed during preflight: …

Preflight could not measure the block interval.

Read the cause after the calibration message and check the endpoint. Check the `<target endpoint="ws://…">` value, and that the node is running and reachable.

### Node CPU and memory are not recorded

PolkaMeter samples only a node process on its own machine. See [Node CPU and memory](how-it-works.md#node-cpu-and-memory).

### blocks: RPC connection lost (…); reconnected after \<seconds> s, nothing observed meanwhile

A connection to the node dropped, often at a proxy idle timeout, and PolkaMeter reconnected. If it happens often, raise the proxy's or load balancer's idle timeout above the run's length. See [Why a dropped connection is not a chain failure](how-it-works.md#why-a-dropped-connection-is-not-a-chain-failure).

### generator limit: the machine was suspended for \<seconds> s

The machine running PolkaMeter slept, or its VM was paused, during the load. The chain went on without load, so the numbers after it are not the chain's.

Keep the machine awake during the run. On a CI runner, check for host maintenance or an overloaded host.

### run failed: insufficient probe reserve

The output that `probes-ref` names holds fewer transactions than the baseline and recovery probes need. With the defaults, the run needs 165.

Pass `ref="run.probeBudget"` as the plugin's probe count, as [Run a load test](load-test.md#write-a-plan) shows. [`<load>`](plans.md#load) gives the formula.

### run failed: duplicate transaction in flood/probe sources

The same transaction appears in both the flood and the probes, or twice in one list.

Use separate prepared transactions for the flood and probes, for example from a signer the flood does not use. After a connection drops, the load sender can resend a transaction whose reply is missing. The ledger still counts it once.

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

For `1010` and `1014`, rebuild the transactions against the current chain: fresh nonces, funded signers and the current runtime version. For `1016`, lower the rate or raise the node's pool limits. For other codes, read the node's log at the time of the refusals.
