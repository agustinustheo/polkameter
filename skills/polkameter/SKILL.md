---
name: polkameter
description: Sets up and runs PolkaMeter load tests (stress tests) for Polkadot SDK (Substrate) chains, parachains and runtimes. Installs the CLI and the ready-made substrate plugin or scaffolds a custom plugin, generates and validates an XML plan for the user's chain, runs it against a local dev chain, reads the verdict, and adds a GitHub Actions or GitLab CI job. Use when the user wants to load-test, stress-test or benchmark transaction throughput of a Polkadot SDK chain, or mentions PolkaMeter or .polkameter.xml plans.
---

# PolkaMeter

PolkaMeter sends prepared transactions to a Polkadot SDK chain at controlled rates and writes a verdict: `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. A plugin prepares the transactions. Use the substrate plugin, which the install script and release archives install, when the runtime meets its [compatibility requirements](https://agustinustheodorus.com/polkameter/substrate-plugin.html#limits).

Docs: https://agustinustheodorus.com/polkameter/ (LLM index: https://agustinustheodorus.com/polkameter/llms.txt).

## Workflow

1. Install. Check what is there:

   ```sh
   polkameter --version                  # needs 0.3.0 or newer
   command -v polkameter-substrate-plugin
   ```

   If either is missing or older, install the release with both:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
   export PATH="$HOME/.local/bin:$PATH"
   ```

   `cargo install --locked polkameter` alone never installs the plugin. Register the plugin after every install, upgrade or rebuild:

   ```sh
   polkameter plugin install "$(command -v polkameter-substrate-plugin)"
   ```

2. Get a chain. Use the user's node if one is running (`ws://127.0.0.1:9944` is typical). Otherwise fetch the dev chain script, from the tag that matches `polkameter --version`, into the repository root:

   ```sh
   curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/v0.3.0/scripts/dev-chain.sh
   bash dev-chain.sh start      # waits about 2 minutes for 21 blocks
   ```

   Set `POLKAMETER_DEV_RUNTIME=<path>.wasm` to start the user's own runtime. Add `.polkameter/` to `.gitignore`. Never load a public network or a node the user does not control.

3. Pick the plugin. Run `polkameter init --rpc <ws-endpoint> --output plans/load.polkameter.xml`. It checks the runtime's calls, measures the block time and uses the credential profile `substrate-dev`.
   - If it succeeds, use the substrate plugin. Add `--kind transfers` to load `Balances` instead of remarks.
   - If it fails with `has no System.remark`, or preflight fails with `cannot sign transactions for this runtime`, the runtime needs its own plugin. Follow https://agustinustheodorus.com/polkameter/writing-a-plugin.html and start from [templates/custom-plugin.rs](templates/custom-plugin.rs).

   Without a reachable chain, copy [templates/substrate-remarks.polkameter.xml](templates/substrate-remarks.polkameter.xml) and edit the endpoint, `block-interval-seconds` and the funding amount.

   Keep the plan's `<plugin id="substrate" version="...">` equal to `polkameter --version`, here and in CI. Then check the plan:
   - The prepare step's `count` is at least the sum of rate times seconds over the load.
   - The fund and prepare steps use the same `flood-signers`, `probe-signers` and `derivation`.
   - A mortal prepare step (`mortality` above 0) has `expected-seconds` at least as long as the run after it.
   - The `fund-accounts` amount covers each signer's transfers and fees.

4. Map the secret. The plan names a credential profile, which maps to an environment variable. On a dev chain the funded seed is `//Alice`:

   ```sh
   polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
   export POLKAMETER_SUBSTRATE_SEED='//Alice'
   ```

   On a shared network, ask the user for a funded secret URI. Never write a secret into a plan, a commit or a log.

5. Validate, then run. A `FAIL` or `INCONCLUSIVE` verdict exits `1` by default. Add `--fail-on error` while exploring. Read the verdict separately from the command's exit code.

   ```sh
   polkameter validate plans/load.polkameter.xml
   polkameter preflight plans/load.polkameter.xml
   polkameter run plans/load.polkameter.xml --output runs
   ```

   The `Run <state>. Artifacts: <run directory>` line names the run directory. Read `summary.md` there: `Verdict`, `Failure onset` (the breaking point, the first rate step that failed a limit), `Max sustained` and `Loss check`. If the failure onset says `no measure violated`, no step failed and the run did not find a limit. Raise the ramp's rates, the prepare step's `count` and, for a longer run, `expected-seconds`. Copy the numbers exactly.

6. Add CI. Copy [templates/github-workflow.yml](templates/github-workflow.yml) to `.github/workflows/load-test.yml`, or [templates/gitlab-ci.yml](templates/gitlab-ci.yml) into `.gitlab-ci.yml`. Commit `dev-chain.sh` at the repository root and [templates/substrate-remarks-ci.polkameter.xml](templates/substrate-remarks-ci.polkameter.xml) as `plans/substrate-remarks-ci.polkameter.xml`. Both jobs run the short plan and fail on a `FAIL` verdict. Keep the job's CLI version equal to the plan's `<plugin version>`.

## Fixing common failures

| Message | Fix |
| --- | --- |
| `plugin substrate is not installed on this host` | Run `polkameter plugin install "$(command -v polkameter-substrate-plugin)"`, after step 1 |
| `plugin substrate version mismatch` or `checksum mismatch` | Reinstall the plugin. Set the plan's `<plugin version>` to the version `polkameter plugin list` shows |
| `measured block interval ... differs from configured` | Set `block-interval-seconds` to the measured value |
| `insufficient probe reserve` | Pass `<input name="probes" ref="run.probeBudget"/>` to the prepare step |
| `the signer cannot pay the fees` | Give `fund-accounts` the prepare step's signer inputs, raise its `amount`, or fund the seed account |
| `Nothing was sent; fund the funder` | Fund the funder, or lower `amount` or the number of signers. A rerun spends nothing |
| `less than expected-seconds` or `may have expired; raise mortality` | Raise the prepare step's `mortality` to the suggested value, or set it to `0` |
| `an earlier prepare step of this run also signs with` | Give the second prepare step another `derivation` and its own `fund-accounts` step |
| Load stop `all N prepared transactions sent` | Raise the prepare step's `count` above the sum of rate times seconds |
| `uses profile ..., which is not configured on this host`, or a variable that `is not set` or `is empty` | Map the profile with `polkameter plugin credential` and export its variable |

