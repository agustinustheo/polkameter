---
name: polkameter
description: Sets up and runs PolkaMeter load tests (stress tests) for Polkadot SDK (Substrate) chains, parachains and runtimes. Installs the CLI and the ready-made substrate plugin or scaffolds a custom plugin, generates and validates an XML plan for the user's chain, runs it against a local dev chain, reads the verdict, and adds a GitHub Actions or GitLab CI job. Use when the user wants to load-test, stress-test or benchmark transaction throughput of a Polkadot SDK chain, or mentions PolkaMeter or .polkameter.xml plans.
---

# PolkaMeter

PolkaMeter sends prepared transactions to a Polkadot SDK chain at controlled rates and writes a verdict: `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. A plugin prepares the transactions. Use the substrate plugin when the runtime meets its [compatibility requirements](https://agustinustheodorus.com/polkameter/substrate-plugin.html#limits). The install script and release archives install it.

Docs: https://agustinustheodorus.com/polkameter/ (LLM index: https://agustinustheodorus.com/polkameter/llms.txt).

## Workflow

1. Install. Check what is there:

   ```sh
   polkameter --version                  # needs 0.4.0 or newer
   command -v polkameter-substrate-plugin
   ```

   If either is missing or older, install the release with both:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
   export PATH="$HOME/.local/bin:$PATH"
   ```

   `cargo install --locked polkameter` doesn't install the plugin. [Install](https://agustinustheodorus.com/polkameter/install.html) has the cargo command for it. Register the plugin after every install, upgrade or rebuild:

   ```sh
   polkameter plugin install "$(command -v polkameter-substrate-plugin)"
   ```

2. Get a chain. Use the user's node if one is running (`ws://127.0.0.1:9944` is typical). Otherwise, download the dev chain script into the repository root. Use the tag that matches `polkameter --version`:

   ```sh
   curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/v0.4.1/scripts/dev-chain.sh
   bash dev-chain.sh start      # waits about 2 minutes for 21 blocks
   ```

   Set `POLKAMETER_DEV_RUNTIME=<path>.wasm` to start the user's own runtime. Add `.polkameter/` to `.gitignore`. Never load a public network or a node the user does not control.

3. Pick the plugin. Run `polkameter init --rpc <ws-endpoint> --output plans/load.polkameter.xml`. It checks the runtime's calls, measures the block time and uses the credential profile `substrate-dev`.
   - If it succeeds, use the substrate plugin. Add `--kind transfers` to load `Balances` instead of remarks.
   - If it fails with `has no System.remark`, or preflight fails with `cannot sign transactions for this runtime`, the runtime needs its own plugin. Follow https://agustinustheodorus.com/polkameter/writing-a-plugin.html and start from [templates/custom-plugin.rs](templates/custom-plugin.rs).

   Without a reachable chain, copy [templates/substrate-remarks.polkameter.xml](templates/substrate-remarks.polkameter.xml) and edit the endpoint, `block-interval-seconds` and the funding amount.

   Keep the plan's `<plugin id="substrate" version="...">`, the `dev-chain.sh` tag and the CI `version` equal to `polkameter --version` ([Versions](https://agustinustheodorus.com/polkameter/install.html#versions)). Then check the plan:
   - The prepare step's `count` is at least the sum of rate times seconds over the load.
   - The fund and prepare steps use the same `flood-signers`, `probe-signers` and `derivation`.
   - A mortal prepare step (`mortality` above 0) signs transactions that expire. Its `expected-seconds` covers the whole run after it.
   - The `fund-accounts` amount covers each signer's transfers and fees.

4. Map the secret. The plan names a credential profile, which maps to an environment variable. On a dev chain the funded seed is `//Alice`:

   ```sh
   polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
   export POLKAMETER_SUBSTRATE_SEED='//Alice'
   ```

   On a shared network, ask the user for a funded secret URI. Never write a secret into a plan, a commit or a log.

5. Validate, then run. A `FAIL` or `INCONCLUSIVE` verdict exits `1` by default. Add `--fail-on error` while exploring. The exit code isn't the verdict. Read the verdict in `summary.md`.

   ```sh
   polkameter validate plans/load.polkameter.xml
   polkameter preflight plans/load.polkameter.xml
   polkameter run plans/load.polkameter.xml --output runs
   ```

   The `Run <state>. Artifacts: <run directory>` line names the run directory. Read `summary.md` there: `Verdict`, `Failure onset` (the first rate step that failed a limit), `Max sustained` and `Loss check`. Copy the numbers exactly. If the failure onset says `no measure violated`, the run did not find a limit. Raise the `<ramp>` rates. Raise the prepare step's `count` above the sum of rate times seconds. For a longer run, also raise `expected-seconds`.

6. Add CI. Copy [templates/github-workflow.yml](templates/github-workflow.yml) to `.github/workflows/load-test.yml`, or [templates/gitlab-ci.yml](templates/gitlab-ci.yml) into `.gitlab-ci.yml`. Commit `dev-chain.sh` at the repository root and [templates/substrate-remarks-ci.polkameter.xml](templates/substrate-remarks-ci.polkameter.xml) as `plans/substrate-remarks-ci.polkameter.xml`. Both jobs run the short plan and fail on a `FAIL` verdict.

## Fixing common failures

Most messages name their fix. Otherwise, look the message up in [Troubleshooting](https://agustinustheodorus.com/polkameter/troubleshooting.html). For a load that ended early, see [What to do about INCONCLUSIVE](https://agustinustheodorus.com/polkameter/reading-results.html#what-to-do-about-inconclusive).
