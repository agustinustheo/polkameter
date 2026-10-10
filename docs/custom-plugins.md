# Writing a plugin

PolkaMeter's core is chain-agnostic. It submits prepared extrinsics at a controlled rate, follows the best and finalized blocks, reconciles every transaction and writes the report. It does not know your pallets, your call encodings or the state a load should leave behind. A plugin supplies those. It is an executable that speaks JSON over stdio, usually written with `polkameter-plugin-sdk`.

This page is for an engineer with a custom runtime who wants PolkaMeter to stress it. It covers the plugin API, the smallest plugin in the repository and then a real one: the People chain plugin in [polkadot-pop-e2e PR #39](https://github.com/paritytech/polkadot-pop-e2e/pull/39). That PR is not merged yet, so its links point at its branch. The protocol itself is described on the [Plugins](plugins.md) page.

## Why a plugin

Keeping the split in one place makes clear what you write and what you do not.

| PolkaMeter core | Your plugin |
| --- | --- |
| Submits prepared transactions at the rate the plan sets | Builds, signs and encodes the transactions for your runtime |
| Follows blocks and gives each transaction one status | Prepares the state a load depends on: nonces, proofs, allowances |
| Scrapes node metrics, measures the block interval, applies stop rules | Checks the state the load should leave behind |
| Writes the run directory and the built-in checks | Observes runtime-specific storage and events, and judges them |

A plugin is a separate process, so you can keep it in your own repository and install it without rebuilding PolkaMeter. Installed plugins are trusted code and are not sandboxed, so review them as you would any code that touches your keys.

## Anatomy of a plugin

### Cargo setup

A plugin is a Cargo package with a binary. It depends on `polkameter-plugin-sdk`, which for now is a git dependency pinned to a revision. PR #39 pins the SDK and the library crates it uses to one commit in its workspace manifest:

```toml
polkameter-plugin-sdk = { git = "https://github.com/agustinustheo/polkameter", rev = "bbe2afad61111ee117d553294ce6658a7649b6b7" }
```

The [workspace manifest](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/Cargo.toml) lists the other `polkameter-*` crates the same way. Add the library crates only when you need them. `polkameter-chain` gives you a client and storage reads. `polkameter-monitors` runs observers that record series. `polkameter-files` writes plugin metrics and reads run files. `polkameter-checks` judges a run. [Library crates for plugins](plugins.md#library-crates-for-plugins) lists the items each one exports.

The API on this page is the one at `bbe2afad`, the revision PR #39 pins. It has `Operation::new`, `.read_only()` and `StateCheckInput`. Older revisions such as `3efb54af` lack them, so check the names against the revision you pin.

### main and serve

```rust
#[tokio::main]
async fn main() -> Result<()> {
    polkameter_plugin_sdk::serve(Example).await
}
```

`serve` reads one JSON request per line from standard input and writes one reply per line to standard output. Standard output carries protocol replies only, so write logs to standard error. Standard error is kept in `plugins/<id>/stderr.log` for the run, which is why it must never contain credentials.

### The Plugin trait

```rust
pub trait Plugin {
    fn manifest(&self) -> Manifest;
    fn invoke(
        &mut self,
        operation: &str,
        inputs: Value,
        context: &Context,
    ) -> impl Future<Output = Result<Value>> + Send;
}
```

`manifest` describes what the plugin offers, and `invoke` runs one named operation. Before calling `invoke`, `serve` checks the inputs against the operation's schema, and after it returns it checks the outputs, so a plugin returns the shape it declared. One process lives for the whole run and calls are serialized. Keep any per-user or per-iteration state keyed by the `Context`, which carries `run_id`, `artifact_dir`, `user` and `iteration`.

### The manifest

Each operation has a description, named inputs and outputs typed by a `Schema`, and a `read_only` flag. This is the `double` operation from the example plugin:

```rust
(
    "double",
    Operation::new(
        "Double a number without chain access",
        &[("value", Schema::Integer)],
        &[("value", Schema::Integer)],
    )
    .read_only(),
),
```

The schemas are `String`, `Number`, `Integer`, `Boolean`, `Array { items }`, `Object { fields }` and `Json`, which accepts any value. A read-only operation may run in live preflight, and a plan's `<preflight>` steps may use only read-only operations. Mark an operation read-only only if it changes nothing on the chain and submits nothing.

The manifest also lists requirements, which the engine checks before setup. Each `Requirement` has a `kind`, a `target`, a `name` and a `required` flag. An `rpc` requirement names an RPC method that must appear in the target node's `rpc_methods`. A `metric` requirement names a metric family that every node of a monitor role must expose. A `capability` requirement names something the provisioner recorded with `polkameter plugin capability NAME`. Any other kind fails when `required` is true; optional requirements are not checked. `polkameter plugin inspect` reports these before a network starts. The example and the People plugin both declare none.

## Start small: the example plugin

[plugins/example](https://github.com/agustinustheo/polkameter/blob/main/plugins/example/src/main.rs) is the smallest complete plugin. It has six operations, most of which exist to exercise failure paths (`crash`, `invalid-output`, `fail-with-message`, `fail` and `wait`). `double` is the one to read. [examples/plugin-workflow.polkameter.xml](https://github.com/agustinustheo/polkameter/blob/main/examples/plugin-workflow.polkameter.xml) uses it in setup:

```xml
<setup>
  <step id="seed" use="example.double"><input name="value" value="21"/></step>
  <step id="copied" use="core.echo"><input name="value" ref="steps.seed.value"/></step>
</setup>
```

`steps.seed.value` names an output the operation declared. Setup outputs are copied into every user and iteration, where the workflow doubles the value again and `core.assert-equal` checks for 84. [Getting started](getting-started.md) builds the example and runs the plan. It needs no chain.

## Worked example: the People plugin

The People plugin in PR #39 floods a fork of previewnet. Its source is in [stress/people-plugin](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/people-plugin), its plans are in [stress/plans](https://github.com/paritytech/polkadot-pop-e2e/tree/feat/th-polkameter-stress-flood/stress/plans), and [stress/README.md](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/README.md) describes the layout. The sections below follow its concerns.

### The manifest and its operations

The plugin registers eight operations. Each is built with `Operation::new`, and read-only ones add `.read_only()`:

```rust
(
    "preflight".into(),
    Operation::new(
        "Check People extension layout and claim encoding",
        &[("target", Schema::String)],
        &[("identity", Schema::Json)],
    )
    .read_only(),
),
```

[main.rs](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/people-plugin/src/main.rs) declares these operations:

| Operation | Read-only | What it does |
| --- | --- | --- |
| `preflight` | yes | Checks the extension layout and claim encoding, and returns the runtime identity |
| `recognize` | no | Prepares signed sudo recognition transactions for setup to submit |
| `prepare-claims` | no | Waits for the rings, then precomputes every claim proof |
| `validate-prepared` | yes | Rejects prepared claims from another runtime or an expired period |
| `check-state` | yes | Checks that the claims of sampled finalized transactions left allowance entries |
| `recycler-start` | yes | Starts recording Recycler maintenance |
| `recycler-stop` | yes | Waits for the Recycler backlog to drain, then stops recording |
| `recycler-checks` | yes | Judges the recorded Recycler series against the run |

### Preparing transactions offline

`recognize` runs in setup. It signs with the credential the plan names as `credentials.admin`. The secret reaches the plugin as an input and is redacted from events. It batches sudo'd `People.force_recognize_personhood` calls 500 keys at a time and returns each one as a `PreparedTx`:

```rust
let tx = GeneralTx::new(&chain, client.sudo(&call).await?)
    .nonce(nonce)
    .sign(&signer);
client.validate(&tx, "recognize people").await?;
txs.push(PreparedTx::new(&tx, json!({"sudo":true})));
nonce = nonce.checked_add(1).context("nonce overflow")?;
```

`prepare-claims` does the expensive work before the load starts. It waits for the rings, proves every claim, validates the first and last transactions against the runtime, and then splits the list. The last `probe-reserve` entries become the probes:

```rust
let (flood, probes) = transactions.split_at(total - reserve);
let state = transactions
    .iter()
    .map(|t| (t.hash.clone(), t.metadata.clone()))
    .collect::<BTreeMap<_, _>>();
```

`probe-reserve` comes from the plan as `run.probeBudget`, the number of probe transactions the run needs. The core sends each hash at most once, so the flood and the probes must never share one. A reply is capped at 8 MiB, so the lists go to the run directory with `Artifact::write` and the operation returns references to them.

`validate-prepared` runs in setup, right after `prepare-claims` (the plan's `fresh` step). It compares the genesis hash, spec version and transaction version recorded with the claims (`steps.claims.identity`) with the live chain, and it checks that the claims' period has not passed. A prepared claim expires with its period, so the run stops before the load rather than submitting stale transactions.

### The state check

After recovery, the core samples finalized transactions and calls the operation named by `state-check`. The plan passes it the prepared state with `state-ref`, and the operation receives `target`, `state`, `hashes` and `at`. The SDK's `StateCheckInput` describes that input, and the People plugin deserializes it with it.

The plugin maps each sampled hash to the target it was prepared for, then asks the finalized block whether that target has an allowance entry:

```rust
if !has_prefix::<([u8; 32], ScaleValue), _>(
    &block,
    "Resources",
    "StmtStoreAllowanceByAccount",
    (target,),
)
.await?
{
    missing += 1;
}
```

It returns `checked`, `missing` and a `detail` string, which are the three outputs the manifest declares. The state artifact is cached and reloaded when its path or hash changes. If a cached connection drops during a long run, the plugin reconnects and retries once. That is safe because each read is at a fixed block hash.

### A runtime-specific observer and its checks

The Recycler is People's maintenance logic. The [observer](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/people-plugin/src/recycler/monitor.rs) records what it does during a run, and the [checks](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/people-plugin/src/recycler/checks.rs) judge that recording. `recycler-start` runs in setup. It writes `metrics.json` and `series.jsonl` into the plugin's directory, and the run reads them with its own series under `job="plugin"`. Each metric is declared with `Metric::new`. `def` here is the People plugin's own helper in `recycler/metrics.rs` that builds the `Def`:

```rust
pub const RECYCLER_QUEUED: Metric<kind::Gauge, 1> = Metric::new(def(
    "people_recycler_queued_keys",
    Kind::Gauge,
    &["collection"],
    &[],
    "Keys in the onboarding queue, not yet in a ring (Members.OnboardingQueue).",
));
```

The recorder implements `walker::Walk`, which runs over every finalized block. It counts maintenance calls by result:

```rust
if MAINTENANCE_PALLETS.contains(&pallet.as_str()) && call.ends_with("_authorized") {
    let result = if failed.contains(&(i as u32)) { "failed" } else { "success" };
    self.series.inc(
        &MAINTENANCE_CALLS,
        [&format!("{pallet}.{call}"), result],
        1.0,
        now_ms(),
    );
}
```

`recycler-stop` runs in evaluate. It waits up to `drain-ms` for the backlog to return to where it started, then stops the walker and returns what it could not record. `recycler-checks` takes that list and the run directory, and returns the checks as its `checks` output. The four checks come from one table:

```rust
const CHECKS: [(&str, bool, Run); 4] = [
    ("the backlog clears", false, backlog_clears),
    ("maintenance keeps up", false, maintenance_keeps_up),
    ("cleanup keeps up", false, cleanup_keeps_up),
    ("time from load to built root", true, load_to_root),
];
```

The middle field marks a check as optional, so smoke mode does not fail when it has no result. A fifth result, "the Recycler observer recorded everything", is added from the problems list. On a chain without a Members pallet the observer records nothing. The four Recycler checks report no result, and the fifth reports pass.

### The plans

The People plugin ships three plans. They differ in size, load and mode, and the topology is set by the CI job, not the plan.

| Plan | Mode | People and slots | Load | Baseline and recovery probes | Block interval | Fork cores and collators |
| --- | --- | --- | --- | --- | --- | --- |
| [`people-smoke`](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/plans/people-smoke.polkameter.xml) | smoke | 40 and 12 | rate steps of 2, 4 and 6 tx/s, 30 s each | 3 and 120 s | 6 s | 1 and 1 |
| [`people-default`](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/plans/people-default.polkameter.xml) | stress | 750 and 20 | ramp from 6 tx/s, adding 4 per step, ten 60 s steps | 5 and 900 s | 2 s | 3 and 5 |
| [`people-capacity`](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/plans/people-capacity.polkameter.xml) | stress | 250 and 20 | rate steps of 12, 15, 18 and 21 tx/s, 60 s each | 5 and 900 s | 2 s | 3 and 5 |

The default plan's load is a ramp, and its monitors name the topology alias, the relay target and the parachain:

```xml
<load target="people" source-ref="steps.claims.flood" probes-ref="steps.claims.probes" state-check="people.check-state" state-ref="steps.claims.state" block-interval-seconds="2">
  <ramp start="6" step="4" steps="10" seconds="60"/>
</load>
<monitors topology="previewnet" relay-target="relay" para-id="1502">
```

The `claims` step in setup passes `run.probeBudget` as `probe-reserve`, which is how the probes are sized. Setup outputs are visible to evaluate and teardown, and the `load` element picks the flood, the probes and the state from that step. The credential is declared as `<credential id="admin" profile="previewnet-sudo"/>`, so the plan names a profile and the host supplies the secret.

### Continuous integration

[stress-flood.yml](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/.github/workflows/stress-flood.yml) runs the whole flow on a fresh fork:

1. It checks that `POLKAMETER_REF` is a 40-character commit hash, then checks out Polkameter at that commit and the engine that bites the fork.
2. It builds the `polkameter` CLI without its desktop app (`--no-default-features`) and the People plugin, then runs `polkameter plugin install` on the plugin and `polkameter plugin credential previewnet-sudo POLKAMETER_SETUP_SURI`.
3. It picks cores and collators from the plan name, copies the plan into the results directory and saves `polkameter plugin inspect` output as `requirements.json`.
4. It bites the fork with those cores and collators, retrying up to three times. It then checks the fork against the requirements with `verify-previewnet-requirements.py`.
5. It starts the network, waits until every chain produces and finalizes blocks, and waits two more minutes. The block interval is measured over the last 60 blocks, and a fresh fork's first blocks are uneven.
6. It runs `polkameter plugin topology previewnet "$ZOMBIE_JSON"`, then `polkameter run "$PLAN" --output "$RESULTS_DIR"`.
7. It writes `summary.md` to the job summary, uploads the results and node logs, and stops the network.

Stress mode is a measurement. The job fails only when the network or the tools fail, and failed health checks are reported in the summary. Smoke mode fails on any gap. The five `polkameter-*` revs in `stress/Cargo.toml` and `POLKAMETER_REF` in the workflow must be bumped together, as [stress/README.md](https://github.com/paritytech/polkadot-pop-e2e/blob/feat/th-polkameter-stress-flood/stress/README.md) explains.

## Install, pin and run your plugin

Build the plugin, install it, then run the plan through each stage. Only `run` touches the chain's transactions:

```sh
cargo build --release --bin my-runtime-plugin
polkameter plugin install target/release/my-runtime-plugin
polkameter plugin credential my-profile MY_SETUP_SURI
polkameter plugin topology my-network /path/to/zombie.json
polkameter plugin inspect plans/smoke.polkameter.xml
polkameter validate plans/smoke.polkameter.xml
polkameter preflight plans/smoke.polkameter.xml
polkameter run plans/smoke.polkameter.xml --output runs
```

- `plugin install` reads the manifest and records the plugin's ID, version and the BLAKE2 hash of its executable. Reinstall after every rebuild.
- A plan declares each plugin as `<plugin id="..." version="..." protocol="1"/>`. The host requires the installed version to match exactly, and protocol `1`.
- `plugin credential PROFILE ENV` maps a profile that the plan names to an environment variable on this host. On `preflight` and `run`, `--credential-env` overrides it for a plan with exactly one credential.
- `plugin topology NAME PATH` registers a Zombienet `zombie.json` under an alias. The plan refers to the alias, not the path.
- `plugin inspect` works offline. It prints the manifests, the required evidence, the targets and the monitor requirements, so a provisioner can read them before starting a network.
- `validate` checks the plan without starting plugins. `preflight` starts the plugins, checks the requirements and runs the read-only preflight steps. `run --output` performs the load and writes the run directory.
- `POLKAMETER_PLUGIN_REGISTRY` moves the registry away from the default `~/.config/polkameter/plugins.json`. The CI job keeps its registry in the workspace.
- For a remote agent, install the plugin and the profiles on the worker. The client sends plan XML only.

The protocol version is 1, and `serve` rejects requests for any other. A crash, a malformed reply or an operation error ends the plugin process, and the host does not retry the operation. Run `preflight` and a smoke plan on a fork before a stress run.

## Checklist for your runtime

- Write down the load: which call, which signer, how many transactions, and what each one needs from the chain, such as a nonce, a proof, an allowance or a queue position.
- Prepare the transactions in an operation before the run, and return them as `PreparedTx` values. Use an `Artifact` once a list grows, because a reply is capped at 8 MiB.
- Keep the flood and the probes disjoint, and size the probe reserve from `run.probeBudget`.
- Add a freshness check that compares the runtime identity (genesis hash, spec version and transaction version) and any expiry in the prepared data.
- Check the layout you rely on before setup, as the People `preflight` does with its extension and encoding checks.
- Declare a `state-check` for the state the load should leave behind, read at the block `at` that the core passes in.
- For any runtime-specific observer, start it in setup, stop it in evaluate, and declare its metrics in `metrics.json`.
- Give each check an outcome and a status. Mark a check optional only when smoke mode should not fail without its result.
- Declare the RPC methods, metrics and capabilities your plugin needs as requirements, so `preflight` and `run` fail before setup. `plugin inspect` lists them for the provisioner but does not check them.
- Keep secrets in credential profiles, and keep them out of stdout and stderr.
- Pin both sides: the SDK revision in `Cargo.toml`, and the checksum that `plugin install` records.
- Run `validate`, `plugin inspect` and `preflight` against a fork, then a smoke plan, and only then a stress plan.
