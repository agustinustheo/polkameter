---
name: polkameter
description: Sets up and runs PolkaMeter load tests (stress tests) for Polkadot SDK (Substrate) chains, parachains and runtimes. Installs the CLI and the ready-made substrate plugin or scaffolds a custom plugin, generates and validates an XML plan for the user's chain, runs it against a local dev chain, reads the verdict, and adds a GitHub Actions or GitLab CI job. Use when the user wants to load-test, stress-test or benchmark transaction throughput of a Polkadot SDK chain, or mentions PolkaMeter or .polkameter.xml plans.
---

# PolkaMeter

PolkaMeter submits prepared transactions to a Polkadot SDK chain at controlled rates, follows the chain, and writes a verdict (`PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`) with JMeter-compatible results. A plugin prepares the transactions. The substrate plugin, shipped with PolkaMeter from v0.3.0, works on any chain with the standard `System` and `Balances` pallets, so most users need no Rust code.

Docs: https://agustinustheodorus.com/polkameter/ (LLM index: https://agustinustheodorus.com/polkameter/llms.txt).

## Workflow

Work through [checklist.md](checklist.md) and tick items as you go. In short:

1. **Install.** Check what is there:

   ```sh
   polkameter --version                  # needs 0.3.0 or newer
   command -v polkameter-substrate-plugin
   ```

   If either is missing or the version is older than 0.3.0, install the release, which holds the CLI and the substrate plugin:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
   export PATH="$HOME/.local/bin:$PATH"
   ```

   <!-- v0.3.0: remove after release -->
   **Unreleased:** until v0.3.0 is released, the install script installs v0.2.0, which has no substrate plugin and no `init`. Build both from the repository instead (replace `main` with the release tag once v0.3.0 is out). The same command works for a platform without a release archive:

   ```sh
   cargo install --locked --git https://github.com/agustinustheo/polkameter --branch main polkameter polkameter-substrate-plugin
   ```

   `cargo install --locked polkameter` alone never installs the plugin. Then always register the plugin with PolkaMeter, also after every upgrade or rebuild:

   ```sh
   polkameter plugin install "$(command -v polkameter-substrate-plugin)"
   ```

2. **Get a chain.** Use the user's node if one is running (`ws://127.0.0.1:9944` is typical). Otherwise start the dev chain recipe (pinned, checksummed `polkadot-omni-node` with the parachain template runtime, or the user's own runtime via `POLKAMETER_DEV_RUNTIME=<path>.wasm`). Fetch `dev-chain.sh` into the user's repository root from the PolkaMeter version you installed:

   <!-- v0.3.0: remove after release (replace main with v0.3.0) -->
   ```sh
   curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/main/scripts/dev-chain.sh
   bash dev-chain.sh start      # waits about 2 minutes for 21 blocks
   ```

   It keeps its files in `.polkameter/dev-chain`; add `.polkameter/` to the user's `.gitignore`. Never point a load test at a public network or a node the user does not control.

3. **Pick the plugin.** Run `polkameter init --rpc <ws-endpoint> --output plans/load.polkameter.xml`. It checks the chain for `System.remark` and `Balances.transfer_keep_alive`, measures the block time, sets a mortality that covers the run, and names the credential profile `substrate-dev`.
   - It succeeds: use the substrate plugin. Use `--kind transfers` to load `Balances` instead of remarks.
   - It fails with `has no System.remark`, or preflight fails with `cannot sign transactions for this runtime` (for example an `AccountId20`/ECDSA runtime, or a transaction extension that carries data): the runtime needs its own plugin. Follow https://agustinustheodorus.com/polkameter/writing-a-plugin.html and start from [templates/custom-plugin.rs](templates/custom-plugin.rs).

   Without a reachable chain, copy [templates/substrate-remarks.polkameter.xml](templates/substrate-remarks.polkameter.xml) and edit the endpoint, `block-interval-seconds` and the funding amount.

   Keep the plan's `<plugin id="substrate" version="...">` equal to the installed CLI's version (`polkameter --version`), here and in CI: the plugin ships with the CLI.

4. **Map the secret.** The plan names a credential profile; the secret stays in an environment variable. On a dev chain the funded seed is `//Alice`:

   ```sh
   polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
   export POLKAMETER_SUBSTRATE_SEED='//Alice'
   ```

   On a shared network, ask the user for a funded secret URI and keep it in their secret store. Never write a secret into a plan, a commit or a log.

5. **Validate, then run.** While you explore the chain's limit, run without `--fail-on-verdict`: a `FAIL` verdict is a finding, not an error.

   ```sh
   polkameter validate plans/load.polkameter.xml
   polkameter preflight plans/load.polkameter.xml
   polkameter run plans/load.polkameter.xml --output runs
   ```

   The run's last line is `Run completed. Artifacts: <run directory>` (or `Run completed_with_failures. Artifacts: ...`). Read `summary.md` in that directory: the **Verdict**, **Failure onset** (the first rate step that failed), **Max sustained** rate and the **Loss check**. If the failure onset says `no measure violated`, no step failed and the run did not find the chain's limit: raise the ramp's rates and the prepare step's `count`, and raise `expected-seconds` if the run gets longer. Report the numbers to the user exactly as written; do not invent them.

6. **Add CI.** Copy [templates/github-workflow.yml](templates/github-workflow.yml) to `.github/workflows/load-test.yml`, or [templates/gitlab-ci.yml](templates/gitlab-ci.yml) into `.gitlab-ci.yml`. Commit `dev-chain.sh` to the repository root and [templates/substrate-remarks-ci.polkameter.xml](templates/substrate-remarks-ci.polkameter.xml) as `plans/substrate-remarks-ci.polkameter.xml`. Both jobs start the dev chain, run the short plan with `--fail-on-verdict` (CI is where a `FAIL` should fail the job) and keep the run directory. The GitHub job's `version:` input and the GitLab job's `POLKAMETER_VERSION` pin the CLI; keep them equal to the plan's `<plugin version>`.

## Fixing common failures

| Message | Fix |
| --- | --- |
| `plugin substrate is not installed on this host` | Run `polkameter plugin install "$(command -v polkameter-substrate-plugin)"`; install the plugin first (step 1) if `command -v` finds nothing |
| `plugin substrate version/checksum mismatch` | Reinstall the plugin after every rebuild or upgrade, and set the plan's `<plugin version>` to `polkameter plugin list`'s version |
| `measured block interval ... differs from configured` | Set `block-interval-seconds` to the measured value |
| `insufficient probe reserve` | Pass `<input name="probes" ref="run.probeBudget"/>` to the prepare step |
| `the signer cannot pay the fees` | Give `fund-accounts` the same `flood-signers`, `probe-signers`, `derivation` and `probes` inputs as the prepare step, raise its `amount`, or fund the seed account |
| `Nothing was sent; fund the funder` | The funder cannot pay the top-ups, the fees and its existential deposit: fund it, or lower `amount` or the number of signers. Running it again spends nothing |
| `less than expected-seconds` or `may have expired; raise mortality` | Raise the prepare step's `mortality` to the suggested value, or set it to `0` |
| `an earlier prepare step of this run also signs with` | Give the second prepare step another `derivation`, and fund it with its own `fund-accounts` step |
| Load stop `all N prepared transactions sent` | Raise the prepare step's `count` above the sum of rate times seconds |
| `uses profile ..., which is not configured on this host`, or a variable that `is not set` or `is empty` | Map the profile with `polkameter plugin credential` and export its variable |

## Reference

- Substrate plugin operations and inputs: https://agustinustheodorus.com/polkameter/substrate-plugin.html
- Plan elements: https://agustinustheodorus.com/polkameter/plans.html
- Reading results: https://agustinustheodorus.com/polkameter/reading-results.html
- Dev chain and CI recipe: https://agustinustheodorus.com/polkameter/dev-chain.html
