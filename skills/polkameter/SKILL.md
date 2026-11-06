---
name: polkameter
description: Set up and run PolkaMeter load tests (stress tests) for Polkadot SDK (Substrate) chains, parachains and runtimes. Installs the CLI, uses the ready-made substrate plugin or scaffolds a custom plugin, generates and validates an XML plan for the user's chain, runs it against a local dev chain, reads the verdict, and adds a GitHub Actions or GitLab CI job. Use when the user wants to load-test, stress-test or benchmark transaction throughput of a Polkadot SDK chain, or mentions PolkaMeter or .polkameter.xml plans.
---

# PolkaMeter

PolkaMeter submits prepared transactions to a Polkadot SDK chain at controlled rates, follows the chain, and writes a verdict (`PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`) with JMeter-compatible results. A plugin prepares the transactions. The substrate plugin, shipped with PolkaMeter from v0.3.0, works on any chain with the standard `System` and `Balances` pallets, so most users need no Rust code.

Docs: https://agustinustheodorus.com/polkameter/ (LLM index: https://agustinustheodorus.com/polkameter/llms.txt).

## Workflow

Work through [checklist.md](checklist.md) and tick items as you go. In short:

1. **Install.** Check `polkameter --version`. If it is missing, install the release (CLI and substrate plugin):

   ```sh
   curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
   export PATH="$HOME/.local/bin:$PATH"
   ```

   If no release archive exists for the platform, build both from source:
   `cargo install --locked --git https://github.com/agustinustheo/polkameter polkameter polkameter-substrate-plugin`.
   Then `polkameter plugin install "$(command -v polkameter-substrate-plugin)"`.

2. **Get a chain.** Use the user's node if one is running (`ws://127.0.0.1:9944` is typical). Otherwise start the dev chain recipe (pinned, checksummed `polkadot-omni-node` with the parachain template runtime, or the user's own runtime via `POLKAMETER_DEV_RUNTIME=<path>.wasm`):

   ```sh
   curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/main/scripts/dev-chain.sh
   bash dev-chain.sh start      # waits about 2 minutes for 21 blocks
   ```

   Never point a load test at a public network or a node the user does not control.

3. **Pick the plugin.** Run `polkameter init --rpc <ws-endpoint> --profile substrate-dev --output plans/load.polkameter.xml`. It checks the chain for `System.remark` and `Balances.transfer_keep_alive` and measures the block time.
   - It succeeds: use the substrate plugin. Use `--kind transfers` to load `Balances` instead of remarks.
   - It fails with `has no System.remark` or the plugin's preflight fails with `cannot sign transactions for this runtime`: the runtime needs its own plugin. Follow https://agustinustheodorus.com/polkameter/writing-a-plugin.html and start from [templates/custom-plugin.rs](templates/custom-plugin.rs).

   Without a reachable chain, copy [templates/remarks.polkameter.xml](templates/remarks.polkameter.xml) and edit the endpoint, `block-interval-seconds` and the funding amount.

4. **Map the secret.** The plan names a credential profile; the secret stays in an environment variable. On a dev chain the funded seed is `//Alice`:

   ```sh
   polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
   export POLKAMETER_SUBSTRATE_SEED='//Alice'
   ```

   On a shared network, ask the user for a funded secret URI and keep it in their secret store. Never write a secret into a plan, a commit or a log.

5. **Validate, then run.**

   ```sh
   polkameter validate plans/load.polkameter.xml
   polkameter preflight plans/load.polkameter.xml
   polkameter run plans/load.polkameter.xml --output runs --fail-on-verdict
   ```

   Read `runs/run-*/summary.md`: the **Verdict**, **Failure onset** (the first rate step that failed), **Max sustained** rate and the **Loss check**. If the failure onset says `no measure violated`, no step failed and the run did not find the chain's limit: raise the ramp's rates and the prepare step's `count`. Report the numbers to the user exactly as written; do not invent them.

6. **Add CI.** Copy [templates/github-workflow.yml](templates/github-workflow.yml) to `.github/workflows/load-test.yml`, or [templates/gitlab-ci.yml](templates/gitlab-ci.yml) into `.gitlab-ci.yml`. Both start the dev chain, run a short plan with `--fail-on-verdict` and keep the run directory. Use a short plan in CI, like [templates/remarks-ci.polkameter.xml](templates/remarks-ci.polkameter.xml).

## Fixing common failures

| Message | Fix |
| --- | --- |
| `plugin substrate version/checksum mismatch` | Reinstall the plugin after every rebuild, and set the plan's `<plugin version>` to `polkameter plugin list`'s version |
| `measured block interval ... differs from configured` | Set `block-interval-seconds` to the measured value |
| `insufficient probe reserve` | Pass `<input name="probes" ref="run.probeBudget"/>` to the prepare step |
| `the signer cannot pay the fees` | Add or raise the `fund-accounts` step's `amount`, or fund the seed account |
| Load stop `all N prepared transactions sent` | Raise the prepare step's `count` above the sum of rate times seconds |
| `credential profile ... is unavailable` or `empty credential profile` | Map the profile with `polkameter plugin credential` and export its variable |

## Reference

- Substrate plugin operations and inputs: https://agustinustheodorus.com/polkameter/substrate-plugin.html
- Plan elements: https://agustinustheodorus.com/polkameter/plans.html
- Reading results: https://agustinustheodorus.com/polkameter/reading-results.html
- Dev chain and CI recipe: https://agustinustheodorus.com/polkameter/dev-chain.html
