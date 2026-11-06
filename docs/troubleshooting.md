# Troubleshooting

Find the message you see below. Each heading is the message as the CLI prints it, without the `PolkaMeter:` prefix. <!-- v0.3.0: remove after release -->The v0.2.0 CLI spells the prefix `Polkameter:`. The entries that start with `error:` are printed by Cargo or by the argument parser.

## Building and installing

### error: rustc 1.92.0 is not supported by the following packages:

**Cause:** The default Rust toolchain is older than 1.93, which PolkaMeter requires. Cargo lists the packages that need a newer version.

**Fix:** Run `rustup update`. Or install the pinned toolchain with `rustup toolchain install 1.93.0`, then build with `cargo +1.93.0 build`.

<!-- v0.3.0: remove after release -->
### error: failed to select a version for the requirement `polkameter-plugin-sdk = "^0.3"`

```text
error: failed to select a version for the requirement `polkameter-plugin-sdk = "^0.3"`
candidate versions found which didn't match: 0.2.0
location searched: crates.io index
required by package `hello-plugin v0.1.0 (~/hello-plugin)`
```

**Cause:** The plugin SDK 0.3 is available from v0.3.0. crates.io has 0.2.0 only.

**Fix:** Depend on a commit of this repository until the release, as [Writing a plugin](writing-a-plugin.md#create-the-crate) shows.

<!-- v0.3.0: remove after release -->
### error: unexpected argument '--junit' found

```text
error: unexpected argument '--junit' found

  tip: to pass '--junit' as a value, use '-- --junit'

Usage: polkameter run --output <OUTPUT> <PLAN>

For more information, try '--help'.
```

**Cause:** `--junit` and `junit.xml` are available from v0.3.0. The published v0.2.0 CLI rejects the flag and exits with code `2`.

**Fix:** Drop `--junit`, or build the CLI from source with `cargo install --locked --git https://github.com/agustinustheo/polkameter --rev 58921b7d506f9987db68fc8084b2fb8f13649f94 polkameter`. Check the version with `polkameter --version`.

## Runs

### run failed: … (details in …/execution.json)

```text
Run failed. Artifacts: ~/polkameter/target/runs/run-…
PolkaMeter: run failed: assertion verified failed: expected 85, got 84 (details in ~/polkameter/target/runs/run-…/execution.json)
```

**Cause:** The run started, but a step, a plugin or a deadline failed it. The text after `run failed:` is the reason. Many reasons have their own entry on this page, such as [`assertion <id> failed`](#assertion-id-failed).

**Fix:** Find the reason on this page. `execution.json` in the run directory holds the full error. If a step failed, `events.jsonl` names it in a `step-finished` event with `"success":false`. A failure outside a step, such as the plan's deadline, has no such event, so read `execution.json`.

The line is cut at 300 characters, and long hashes are shortened to 12. `execution.json` keeps the whole message.

### run failed before setup: … (details in …/execution.json)

```text
Phase: preflight
Run failed. Artifacts: ~/polkameter/runs/run-…
PolkaMeter: run failed before setup: p.xml: line 2, column 12: plugin example is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry ~/.config/polkameter/plugins.json) (details in ~/polkameter/runs/run-…/execution.json)
```

**Cause:** A check before the setup phase failed, so the run submitted nothing to the chain: a credential, a plugin install, a step's inputs, the chain's evidence, a `<preflight>` step or the block interval calibration. The run exits `3`, as `polkameter preflight` does, and skips teardown. `execution.json` has `"failed_phase": "preflight"`.

**Fix:** Find the reason after `run failed before setup:` on this page. Run `polkameter preflight <plan>` to check the fix without starting a run.

### verdict INCONCLUSIVE: …; exit 1 because of --fail-on-inconclusive

```text
PolkaMeter: verdict INCONCLUSIVE: required checks with no result: no silent loss; exit 1 because of --fail-on-inconclusive (the checks are in summary.md)
```

**Cause:** The run finished, but its load verdict is `INCONCLUSIVE`, and `--fail-on-inconclusive` makes that exit `1`. The GitHub Action passes the flag by default. The text after `INCONCLUSIVE:` names the stop rule that ended the load early or the required checks with no result.

**Fix:** See [what to do about INCONCLUSIVE](reading-results.md#what-to-do-about-inconclusive). To let the run pass meanwhile, drop the flag, or set `fail-on: verdict` on the action step. A `verdict FAIL: …; exit 1 because of --fail-on-verdict` line works the same way for a `FAIL` verdict.

### smoke mode fails the run on monitor problems and required checks with no result

```text
PolkaMeter: smoke mode fails the run on monitor problems and required checks with no result, whatever the --fail-on flags; fix this one: no silent loss: state check timed out (the checks are in summary.md)
```

**Cause:** A smoke-mode plan (`mode="smoke"`) found a monitor problem or a required check with no result. Smoke mode exits `1` on these whatever the flags, so dropping `--fail-on-inconclusive` or setting `fail-on: verdict` or `fail-on: error` on the action step does not help. The `smoke:` lines before it list every problem.

**Fix:** Fix the monitor or the cause the check's detail names, then run again. See [what to do about INCONCLUSIVE](reading-results.md#what-to-do-about-inconclusive). To let the run pass while the problem stays, run the plan in stress mode.

### teardown failed: teardown was interrupted by a forced stop

```text
PolkaMeter: received SIGTERM; stopping the run (teardown has up to 20 s; send the signal again to stop now)
Phase: teardown
PolkaMeter: stopping now; teardown is interrupted
Run stopped. Artifacts: ~/polkameter/runs/run-…
```

**Cause:** The run was stopped, and then a second signal arrived, or teardown took longer than `--shutdown-grace` (20 seconds by default). PolkaMeter killed the plugins before teardown finished, so cleanup on the chain may be incomplete. CI systems often send SIGINT and SIGTERM a few seconds apart when a job is cancelled.

**Fix:** Raise `--shutdown-grace` to cover your teardown steps, and give the job that long plus 10 seconds between the first signal and SIGKILL. Then check the chain for state that teardown would have removed.

## Plans

Plan errors start with the plan file and, when the error is about one element, its line and column: `PolkaMeter: plan.polkameter.xml: line 6, column 5: …`. The headings below write the file as `<plan>`.

### \<plan>: line 6, column 5: expected \</setup>, found \</polkameter-plan>; \<setup> was opened at line 3, column 3

**Cause:** The XML is not well formed. Here, `<setup>` is never closed.

**Fix:** Go to the line and column where the element was opened, and close it in the right place.

### \<plan>: line 1, column 1: missing `name` on \<polkameter-plan>

**Cause:** A required attribute is missing from the root element. The root needs `version`, `xmlns` and `name`.

**Fix:** Add the attribute. Use `version="1"` and `xmlns="https://polkameter.dev/schema/plan"`.

### \<plan>: line 2, column 3: unknown element \<setpu> in \<polkameter-plan>; did you mean \<setup>? \<polkameter-plan> may contain …

```text
PolkaMeter: el.xml: line 2, column 3: unknown element <setpu> in <polkameter-plan>; did you mean <setup>? <polkameter-plan> may contain <plugins>, <targets>, <credentials>, <preflight>, <setup>, <workflow>, <load>, <monitors>, <thresholds>, <evaluate>, <teardown>
```

**Cause:** An element name is not in the plan schema. The message suggests a close match when there is one, and lists the elements allowed there.

**Fix:** Use one of the listed names. The [plan reference](plans.md) describes each element.

### \<plan>: line …: unknown attribute `user` on \<workflow>; did you mean `users`? \<workflow> allows …

```text
PolkaMeter: at.xml: line 2, column 20: unknown attribute `user` on <workflow>; did you mean `users`? <workflow> allows `id`, `users`, `iterations`, `concurrency`
```

**Cause:** The element has no attribute with that name.

**Fix:** Use one of the listed attributes.

### \<plan>: line …: `users` on \<workflow> must be a whole number from 0 to 4294967295, found `ten`

**Cause:** A numeric attribute holds text that is not a number of the right kind. Whole-number attributes take digits only, with no sign, decimal point or unit.

**Fix:** Write the number in digits, such as `users="10"`. The element's limits are in the [plan reference](plans.md).

### \<plan>: line …: unterminated entity reference: `&` starts a reference such as `&lt;`; write a literal `&` as `&amp;`

```text
PolkaMeter: amp.xml: line 1, column 84: unterminated entity reference: `&` starts a reference such as `&lt;`; write a literal `&` as `&amp;`
```

**Cause:** An attribute value has an `&` that does not start a reference such as `&amp;`. In XML, `&` always starts one.

**Fix:** Write `&amp;` for each `&` in the value, as in `name="Fish &amp; chips"`. A misspelt reference, such as `&nbsp;`, gets `invalid entity` instead; XML has only `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;` and character references such as `&#65;`.

### \<plan>: line …: step `<id>` (\<operation>): input `<name>` refers to `steps.nope.value`, but nothing in the plan defines it; …

```text
PolkaMeter: ref.xml: line 3, column 34: step `a` (core.echo): input `value` refers to `steps.nope.value`, but nothing in the plan defines it; use targets.<id>, credentials.<id>, run.id, run.directory, run.probeBudget, steps.<earlier step>.<output>, or in a workflow user.index or iteration.index
```

**Cause:** A `ref` names a step that does not exist, or a value the plan does not declare.

**Fix:** Correct the step ID or the value's name. The message lists the names a reference can use.

### \<plan>: line …: step `<id>` (\<operation>): input `<name>` refers to `steps.<other>.<output>`, but step `<other>` does not run before this step; …

**Cause:** The referenced step exists, but it runs later, or it is in a workflow and this step is outside it.

**Fix:** Move the referenced step earlier. A workflow's outputs are visible only to later steps of the same workflow.

### \<plan>: line …: `--` is not allowed inside a comment, and a comment cannot end with `--->`

**Cause:** XML forbids `--` inside `<!-- … -->`. Other XML tools reject the file too.

**Fix:** Remove the double hyphen from the comment, or put a space between the hyphens.

### \<plan>: line …: character U+0000 is not allowed in XML; remove it from the plan

**Cause:** The file holds a character that XML 1.0 does not allow, such as NUL or another control character. Editors and generated files sometimes add them invisibly.

**Fix:** Go to the line and column and delete the character. Tab, line feed and carriage return are allowed.

### \<plan>: line 1, column …: invalid encoding `…` in the XML declaration; …

```text
PolkaMeter: enc.xml: line 1, column 31: invalid encoding `ISO-8859-1` in the XML declaration; the plan has non-ASCII text, which PolkaMeter reads as UTF-8; write encoding="UTF-8" (the file already is UTF-8), or remove encoding
```

**Cause:** The XML declaration names an encoding PolkaMeter does not read, or it is malformed. PolkaMeter reads plans only as UTF-8. A declaration of `US-ASCII` or `ISO-8859-1` is accepted while the plan is plain ASCII, which reads the same in all three. Once the plan holds other characters, such as `é`, PolkaMeter refuses it instead of guessing.

**Fix:** Save the file as UTF-8 and write `<?xml version="1.0" encoding="UTF-8"?>`, or remove the declaration.

### plan \<plan> is UTF-16 text; save it as UTF-8, the encoding PolkaMeter reads

**Cause:** The file is UTF-16, which some Windows editors and PowerShell's `>` write. PolkaMeter reads plans only as UTF-8.

**Fix:** Save the file as UTF-8. In PowerShell, use `Out-File -Encoding utf8` or `Set-Content -Encoding utf8`.

### \<plan>: line …: the plan is not UTF-8 text; save it as UTF-8, the encoding PolkaMeter reads

```text
PolkaMeter: latin.xml: line 2, column 6: the plan is not UTF-8 text; save it as UTF-8, the encoding PolkaMeter reads
```

**Cause:** The file holds bytes that are not UTF-8, usually a file saved as Latin-1 or Windows-1252 with an accented letter. The line and column point at the first such byte.

**Fix:** Save the file as UTF-8.

### plan \<plan> is larger than the 2 MiB (2097152 bytes) limit; split the plan or remove unused content

**Cause:** The plan file is over 2 MiB. PolkaMeter stops reading one byte past the limit.

**Fix:** Move large data, such as prepared transactions, out of the plan and into a plugin's output.

### plan \<plan> is empty; if it comes from a pipe or FIFO, check that the command writing it succeeded

**Cause:** The file has no content. A pipe, such as `<(envsubst < plan.tmpl)` or `/dev/stdin`, reads as empty when the command that writes it fails or writes nothing. A named pipe (FIFO) that no process writes to also reads as empty, instead of waiting for a writer.

**Fix:** Check the command that produces the plan. Run it on its own to see its output and errors.

### cannot read plan \<plan>: it is a socket or device, not a plan file; pass a plan file such as plan.polkameter.xml

**Cause:** The path is a socket or a device, such as `/dev/null` or a terminal. PolkaMeter reads plans from regular files and pipes only. A directory gets `it is a directory`.

**Fix:** Pass the plan file. To pass generated text, use a pipe: `generate-plan | polkameter validate /dev/stdin`.

### assertion \<id> failed

**Cause:** A `core.assert-equal` step found that `actual` and `expected` differ. The run fails with exit `1`, and so does a CI job.

**Fix:** Correct `expected`, or fix the value that the step computes. The message names the step.

### cannot read … No such file or directory (os error 2)

```text
PolkaMeter: cannot read run directory target/no-such-run: No such file or directory (os error 2)
```

**Cause:** The path after `cannot read` does not exist. The message says which kind of file PolkaMeter wanted: `plan`, `plugin executable` or `run directory`.

**Fix:** Check the path. For `report`, pass the `run-…` directory itself, the one that holds `execution.json`.

## Plugins

Plugin install errors point at the plan's `<plugin>` element: `PolkaMeter: <plan>: line 2, column 12: plugin …`. `preflight` and `run` exit `3`.

### \<plan>: line …: plugin \<id> is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry …)

```text
PolkaMeter: p.xml: line 2, column 12: plugin example is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry ~/.config/polkameter/plugins.json)
```

**Cause:** The registry file in parentheses has no plugin with that ID. `validate` does not check installs, so it passes.

**Fix:** Run `polkameter plugin install <path-to-executable>` on this host. If the registry path is not the one you installed into, set `POLKAMETER_PLUGIN_REGISTRY` to it on every step that runs `polkameter`.

### \<plan>: line …: plugin \<id> checksum mismatch: if you rebuilt it, run `polkameter plugin install …` again. … changed after it was installed (BLAKE2 expected …, found …)

```text
PolkaMeter: p.xml: line 2, column 12: plugin example checksum mismatch: if you rebuilt it, run `polkameter plugin install /work/hello` again. /work/hello changed after it was installed (BLAKE2 expected 2167d642faa16293c70c23a1aa10faef6b570aad31f24184d40d41727614e789, found 42ab94d08a9bb8cacb18184209ea2e5ffc6fe6f1e731b20e2fff7d66ecb2475b)
```

**Cause:** The executable changed after `plugin install`, usually because it was rebuilt.

**Fix:** Run the `polkameter plugin install` command the message shows, after every rebuild.

### \<plan>: line …: plugin \<id> version mismatch: set version="\<installed>" in the plan's \<plugin>, or install the build the plan expects with `polkameter plugin install <path-to-executable>`. The plan expects version \<version>, but version \<installed> is installed from …

**Cause:** The plan's `<plugin version="…">` differs from the version of the installed executable.

**Fix:** Set the plan's `version` to the installed version, or install the build that has the plan's version with `polkameter plugin install <path-to-executable>`.

### \<plan>: line …: step `<id>` (core.ecco): unknown built-in operation core.ecco; did you mean core.echo? the core operations are …

```text
PolkaMeter: op.xml: line 4, column 5: step `a` (core.ecco): unknown built-in operation core.ecco; did you mean core.echo? the core operations are core.echo, core.assert-equal, core.submit-prepared, core.submit-setup
```

**Cause:** A `core.` operation name is not one of the built-in operations. `validate` checks built-in operations, their inputs and the references between them.

**Fix:** Use one of the listed operations. The [plan reference](plans.md) describes each.

### \<plan>: line …: step `<id>` (\<operation>): input `<name>` refers to `steps.<other>.<output>`; use one of step `<other>`'s outputs: …

```text
PolkaMeter: out.xml: line 4, column 34: step `b` (core.echo): input `value` refers to `steps.a.out`; use one of step `a`'s outputs: `value` (core.echo has no output `out`)
```

**Cause:** The step exists and runs earlier, but its operation has no output with that name.

**Fix:** Use one of the listed outputs. For a plugin operation, `polkameter plugin inspect <plan>` lists each operation's outputs.

### \<plan>: line …: step `<id>` (\<operation>): input `<name>` has value="…", which is …, but \<operation> expects …

```text
PolkaMeter: lit.xml: line 3, column 45: step `a` (core.submit-prepared): input `target` has value="7", which is an integer, but core.submit-prepared expects a string
```

**Cause:** A literal is JSON when it parses as JSON, so `value="7"` is the integer 7, not the text `7`.

**Fix:** Give a value of the expected kind. For text that looks like a number, quote it as JSON: `value="&quot;7&quot;"`.

### \<plan>: line …: step `<id>` (example.nope): unknown operation example.nope: the installed plugin example offers …

**Cause:** The operation is not in the installed plugin's manifest. `validate` does not check it. The message lists the operations the plugin offers.

**Fix:** Fix the name, or reinstall the plugin. `polkameter plugin inspect <plan>` prints each operation's inputs and outputs.

### waiting for \<path>.lock (another polkameter is updating the registry)

**Cause:** Another `polkameter plugin` command holds the registry's lock, for example a parallel CI step installing a plugin. This command waits for it and then carries on.

**Fix:** None is needed. If the line stays, a process is stuck holding the lock: find it with `lsof <path>.lock` (Linux, macOS) and stop it.

### plugin registry \<path> is not a valid registry: …

**Cause:** The registry file is not valid JSON, or has fields PolkaMeter does not know, for example after a manual edit or a full disk.

**Fix:** Fix the file at the path shown. Or move it aside and register this host's plugins, credentials, topologies and capabilities again with `polkameter plugin install`, `plugin credential`, `plugin topology` and `plugin capability`. Concurrent `polkameter plugin` commands are safe: each takes a lock on `<path>.lock` and replaces the registry in one step.

## Credentials and topologies

Credential errors point at the plan's `<credential>` element. `preflight` checks every credential the plan declares, and `preflight` and `run` exit `3`. `polkameter plugin list` shows which variable each profile reads, never a value.

### \<plan>: line …: credential `<id>` uses profile `<profile>`, which is not configured on this host; run `polkameter plugin credential <profile> <ENV_VAR>` to name the environment variable that holds it (registry …)

```text
PolkaMeter: plan.polkameter.xml: line 2, column 16: credential `sudo` uses profile `sudo`, which is not configured on this host; run `polkameter plugin credential sudo <ENV_VAR>` to name the environment variable that holds it (registry ~/.config/polkameter/plugins.json)
```

**Cause:** The registry in parentheses maps no environment variable to the profile.

**Fix:** Run the `polkameter plugin credential` command the message shows. With the GitHub Action, add `<profile>=<ENV_VAR>` to its `credentials` input.

### \<plan>: line …: credential `<id>` (profile `<profile>`) reads environment variable \<ENV_VAR> (mapped by `polkameter plugin credential`), which is not set; …

```text
PolkaMeter: plan.polkameter.xml: line 2, column 16: credential `sudo` (profile `sudo`) reads environment variable SUDO_SEED (mapped by `polkameter plugin credential`), which is not set; set SUDO_SEED on the step that runs polkameter, or map the profile to another variable with `polkameter plugin credential sudo <ENV_VAR>`
```

**Cause:** The variable is not set in the environment of the process that runs `polkameter`. In CI, a variable set on one step is not set on the next. The message says `which is empty` when the variable is set but empty, as an unset secret often is, and `which is not valid Unicode` for bytes that are not text.

**Fix:** Set the variable on the step that runs `polkameter`, for example in the step's `env:` block from a secret. Or map the profile to the variable that holds the secret.

### \<plan>: line …: credential `<id>` (profile `<profile>`) reads environment variable \<ENV_VAR> (from --credential-env), which is not set; …

```text
PolkaMeter: run failed before setup: plan.polkameter.xml: line 2, column 16: credential `sudo` (profile `sudo`) reads environment variable SUDO_SEED_X (from --credential-env), which is not set; set SUDO_SEED_X on the step that runs polkameter, or pass the variable that holds it to --credential-env (details in …/execution.json)
```

**Cause:** The variable that `--credential-env` names is not set, or is empty.

**Fix:** Set the variable on the step that runs `polkameter`, or pass the name of the variable that holds the secret.

### topology alias zombie is not installed on this host

**Cause:** `<monitors topology="zombie">` names an alias that this host has not registered. `preflight` exits `3`.

**Fix:** Run `polkameter plugin topology zombie <path/to/zombie.json>`.

## Chains

### block interval calibration failed during preflight

**Cause:** Preflight could not measure the block interval, usually because the target endpoint is unreachable. The cause is not printed.

**Fix:** Check the `<target endpoint="ws://…">` value, and that the node is running and reachable.

### measured block interval \<measured>s differs from configured \<configured>s by more than 25%; correct the plan before setup

**Cause:** The `block-interval-seconds` attribute on `<load>` differs from the measured block time by more than 25%. Preflight stops before setup.

**Fix:** Set `block-interval-seconds` on `<load>` to the measured value. The default is `6.0`.

### node: ... is not on this machine, so its process cannot be sampled

**Cause:** The load test's target is not a loopback URL, so PolkaMeter cannot tell which local process is the node. The run goes on without CPU or memory.

**Fix:** If the node runs on this machine behind another address, set `POLKAMETER_NODE_PID` to its process ID. Otherwise there is nothing to fix: PolkaMeter samples only local nodes.

### node: port \<port> is served by ssh (PID \<pid>), a port forwarder, not the node

**Cause:** The target URL is a local port that an SSH tunnel or another forwarder serves. Sampling it would measure the forwarder, so PolkaMeter does not.

**Fix:** Run PolkaMeter on the node's machine to record its CPU and memory. The load test itself works through the tunnel.

### blocks: RPC connection lost (...); reconnected after \<seconds> s, nothing observed meanwhile

**Cause:** A WebSocket connection to the node dropped, for example through a proxy idle timeout, and PolkaMeter opened it again. The run went on. The "monitors recorded everything" check warns, and smoke mode exits `1`. If blocks went unobserved for longer than the stall limit in all, the check has no result.

**Fix:** If it happens often, check the proxy or load balancer between PolkaMeter and the node. Raise its idle timeout above the run's length.

### node down: node: process \<pid> ended; sampling the node process now on its port, \<pid> (the node restarted)

**Cause:** The local node's process ID on its port changed. A restart empties the pool, so PolkaMeter stops the load on node down, even when the node was back within seconds. During recovery, it ends recovery.

**Fix:** Check the node's logs for why it restarted, such as an out-of-memory kill or a supervisor restart.

### reconnected to a different node (load balancer, or the node restarted)

**Cause:** The block follower reconnected, and the node had a new peer ID or sync starting block. Behind a load balancer, the reconnect reached another node. Without one, the node restarted. The run goes on, and the verdict and exit code do not change. If the chain stopped producing or finalizing blocks, the stall or finality stall rule stops the load.

**Fix:** With a load balancer, nothing is needed. To measure one node, point the run at that node's URL. Without a load balancer, check the node's logs for why it restarted.

### generator limit: the machine was suspended for \<seconds> s

**Cause:** The machine running PolkaMeter slept or its VM was paused during the load. The chain went on without load, so the numbers after it are not the chain's. A wall-clock step from NTP does not cause this.

**Fix:** Keep the machine awake during the run. On a CI runner, check for host maintenance or an overloaded host.

### run failed: insufficient probe reserve

**Cause:** The output named by `probes-ref` holds fewer transactions than the run needs for baseline and recovery probes. With the defaults, the run needs 165.

**Fix:** Pass `ref="run.probeBudget"` as the plugin's probe count, as [Run a load test](load-test.md#write-a-plan) shows. [Probe budget](plans.md#load) gives the formula.

### run failed: duplicate transaction in flood/probe sources

**Cause:** The same transaction appears in both the flood and the probes, or twice in one list. Each transaction can be sent only once.

**Fix:** Prepare the probes as separate transactions, for example from a signer the flood does not use.

### The node refuses transactions

A node refuses a transaction when its pool will not accept it. The run does not stop on the first refusal. More than 1% refused marks a breaking point, and more than 10% in a step stops the load as `graceful`.

To see why, open these files in the run directory:

- `summary.md`: the `refusals are clean` check counts refusals by error code.
- `steps.jsonl`: each step's `errors` field counts refusals by code and message, as the node sent them.

These are the error codes of a Polkadot SDK transaction pool:

| Code | Meaning | Usual cause |
| --- | --- | --- |
| `1010` | Invalid transaction. The message says why, such as `Transaction is outdated` or `Inability to pay some fees`. | A stale nonce, a signer without funds, or a transaction built for another runtime version. |
| `1012` | Temporarily banned. | The same transaction was recently refused or dropped. |
| `1013` | Already imported. | The transaction was sent twice. |
| `1014` | Priority is too low. | Another transaction with the same signer and nonce is in the pool. |
| `1016` | Immediately dropped. | The pool is full. |

**Fix:** For `1010` and `1014`, rebuild the transactions against the current chain: fresh nonces, funded signers and the current runtime version. For `1016`, lower the rate or raise the node's pool limits. For any other code, read the node's log at the time of the refusals.

## Remote runs

### remote agent does not support --fail-on-verdict; update the agent

**Cause:** The client requested `--fail-on-verdict` or `--fail-on-inconclusive`, and the message names the one that the agent does not advertise. `--fail-on-inconclusive` needs an agent from v0.3.0 or newer. The client stops before starting the run.

**Fix:** Update the agent to the same PolkaMeter ref as the client. Or omit the flag to use the agent's default exit policy.

### remote runner returned 401 Unauthorized: missing or invalid bearer token

**Cause:** The client's token does not match the agent's token.

**Fix:** Set the same value on both sides. The client reads the variable named by `--remote-token-env`. The agent reads the variable named by `--token-env`.

### remote runner returned 400 Bad Request: line …: plugin \<id> is not installed on this host; run `polkameter plugin install <path-to-executable>` (registry …)

**Cause:** The agent's host has no plugin with that ID in the registry in parentheses, which is on the agent's host. The line and column are in the plan the client sent. The client cannot upload plugins.

**Fix:** Install the plugin on the agent host.

### remote runner returned 400 Bad Request: a plugin plan is already running

**Cause:** The agent runs one plan at a time.

**Fix:** Wait for the run to end, or press Ctrl-C in the client that started it. A client that gives up asks the agent to stop its run, so this clears once that run's teardown ends.

### remote runner endpoint must use https://, or http:// only through a loopback SSH tunnel

**Cause:** `--remote` uses plain `http://` with a host that is not loopback. The client refuses it before sending anything.

**Fix:** Use `https://`. Or tunnel to the agent and use `http://127.0.0.1:<port>`.

### remote runner endpoint must not contain user information (user@ or user:password@)

**Cause:** `--remote` has a user name or password before the host, as in `http://127.0.0.1:9901@runner.example`. The real host of such an address is the part after `@`, so it is refused before anything is sent.

**Fix:** Remove everything up to and including `@`. The agent authenticates with the bearer token only.

### status polls kept failing for 60 s: …

```text
PolkaMeter: status poll failed (remote runner request failed: … Connection refused (os error 61)); retrying in 600 ms
...
PolkaMeter: status polls kept failing for 60 s: …; asked the agent to stop run <id>
```

**Cause:** The client lost the agent for a minute: the SSH tunnel closed, the agent restarted, or the agent host is overloaded. The client exits with code `1`. It asks the agent to stop the run first; the message ends with `could not ask the agent to stop run <id>` if that failed too.

**Fix:** Restore the tunnel or the agent. If the agent kept running and could not be told to stop, wait for its run to end before you start another, or send the agent SIGTERM to stop the run cleanly.

### the run did not end within the client's deadline of … s

**Cause:** The remote run lasted longer than the plan's `timeout-ms`, plus its teardown step timeouts, plus one minute. The client asked the agent to stop it and exited with code `1`.

**Fix:** Read the run's `events.jsonl` on the agent to find the step that hung, then fix that step or raise its `timeout-ms`.

### the start request kept failing for 60 s: …

**Cause:** The agent did not accept the start request for a minute: it was unreachable, or it kept answering with a server error. The client asked the agent to stop the run under the ID it chose, in case the run had started, and exited with code `1`.

**Fix:** Restore the tunnel or the agent, then rerun the plan.

### remote runner returned 503 Service Unavailable: the agent is shutting down

**Cause:** The agent received SIGINT or SIGTERM and is stopping its active run. It refuses new runs until it exits.

**Fix:** Start the agent again, then rerun the plan.

### agent token in POLKAMETER_AGENT_TOKEN must have at least 32 characters, at least 8 of them different; generate one with `openssl rand -hex 32`

**Cause:** The agent's bearer token is shorter than 32 characters, or repeats a few characters, as in `aaaa…`. Such a token can be guessed.

**Fix:** Set a random token on the agent and the client:

```sh
export POLKAMETER_AGENT_TOKEN=$(openssl rand -hex 32)
```

### agent binds only to a loopback address; use an SSH tunnel or TLS terminator

**Cause:** `agent serve --bind` names a non-loopback address.

**Fix:** Bind to `127.0.0.1:<port>`, and reach the agent through an SSH tunnel.
