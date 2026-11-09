# PolkaMeter setup checklist

Copy this list into your notes. Tick each item when it is done and checked.

## Install

- [ ] `polkameter --version` prints 0.3.0 or newer.
- [ ] `command -v polkameter-substrate-plugin` prints the installed executable's path.
- [ ] `polkameter plugin install "$(command -v polkameter-substrate-plugin)"` printed `Installed plugin substrate <version> (5 operations)`, after the last install or upgrade.
- [ ] `polkameter plugin list` shows the version the plan's `<plugin version>` names (the CLI's version).

## Chain

- [ ] The endpoint is the user's own node or a local dev chain, never a public network.
- [ ] For a local chain: `dev-chain.sh start` printed `dev-chain: ready at ws://127.0.0.1:9944`, and `.polkameter/` is in `.gitignore`.
- [ ] The runtime has `System.remark` (or `Balances.transfer_keep_alive` for transfers). `polkameter init` checks this.

## Plan

- [ ] Generated with `polkameter init --rpc <endpoint> --output plans/<name>.polkameter.xml`, or copied from `templates/`.
- [ ] `block-interval-seconds` matches the chain's block time.
- [ ] The prepare step's `count` is at least the sum of rate x seconds over the load.
- [ ] The prepare step's `probes` input is `ref="run.probeBudget"`, and so is the `fund-accounts` step's.
- [ ] `fund-accounts` and the prepare step use the same `flood-signers`, `probe-signers` and `derivation`.
- [ ] A mortal prepare step (`mortality` above 0) has `expected-seconds` at least as long as the run after it.
- [ ] The `fund-accounts` amount covers each signer's transfers and fees. The seed account (or `funder`) can pay the total, the fees and its existential deposit.
- [ ] No secret in the plan: the seed comes from `credentials.<id>`.
- [ ] `polkameter validate <plan>` prints `Plan is structurally valid.`

## Run

- [ ] `polkameter plugin credential <profile> <ENV_VAR>` is set, and the variable is exported.
- [ ] `polkameter preflight <plan>` prints `Preflight passed.`
- [ ] `polkameter run <plan> --output runs` ended with `Run completed. Artifacts: <run directory>` or `Run completed_with_failures. Artifacts: <run directory>`.
- [ ] Copied the verdict, breaking point (failure onset), max sustained rate and loss counts from this run's `summary.md`.

## CI

- [ ] The job runs the short CI plan, `plans/substrate-remarks-ci.polkameter.xml`, with `fail-on: verdict` or `--fail-on verdict`.
- [ ] The job pins the CLI (the action's `version:`, or `POLKAMETER_VERSION`) to the plan's `<plugin version>`.
- [ ] `dev-chain.sh` is committed at the repository root, from the same PolkaMeter version.
- [ ] The dev chain binaries are cached, keyed on the hash of `dev-chain.sh`.
- [ ] The run's reports are kept as an artifact, also when the job fails.
- [ ] Third-party actions are pinned to a commit SHA.
