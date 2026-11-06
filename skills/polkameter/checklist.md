# PolkaMeter setup checklist

Copy this list into your notes and tick each item when it is done and checked.

## Install

- [ ] `polkameter --version` prints a version (0.3.0 or newer for the substrate plugin and `init`).
- [ ] `polkameter-substrate-plugin` exists (install script, container, or `cargo install --locked --git https://github.com/agustinustheo/polkameter polkameter-substrate-plugin`).
- [ ] `polkameter plugin install <path>/polkameter-substrate-plugin` printed `Installed plugin substrate <version> (5 operations)`.
- [ ] `polkameter plugin list` shows the version the plan's `<plugin version>` names.

## Chain

- [ ] The endpoint is the user's own node or a local dev chain, never a public network without its operators' permission.
- [ ] For a local chain: `dev-chain.sh start` printed `dev-chain: ready at ws://127.0.0.1:9944`.
- [ ] The runtime has `System.remark` (or `Balances.transfer_keep_alive` for transfers). `polkameter init` checks this.

## Plan

- [ ] Generated with `polkameter init --rpc <endpoint> --profile <profile> --output plans/<name>.polkameter.xml`, or copied from `templates/`.
- [ ] `block-interval-seconds` matches the chain's block time.
- [ ] The prepare step's `count` is at least the sum of rate x seconds over the load.
- [ ] The prepare step's `probes` input is `ref="run.probeBudget"`.
- [ ] The `fund-accounts` amount covers each signer's transfers and fees; the seed account (or `funder`) holds the total.
- [ ] No secret in the plan: the seed comes from `credentials.<id>`.
- [ ] `polkameter validate <plan>` prints `Plan is structurally valid.`

## Run

- [ ] `polkameter plugin credential <profile> <ENV_VAR>` is set, and the variable is exported.
- [ ] `polkameter preflight <plan>` prints `Preflight passed.`
- [ ] `polkameter run <plan> --output runs` ended with `Run completed.`
- [ ] Reported from `summary.md`: verdict, failure onset, max sustained rate, loss check. Numbers copied, not estimated.

## CI

- [ ] The job uses a short plan (one low rate) and `--fail-on-verdict` (or the action's `fail-on: verdict`).
- [ ] The dev chain binaries are cached, keyed on the hash of `dev-chain.sh`.
- [ ] The run directory is uploaded as an artifact, also when the job fails.
- [ ] Third-party actions are pinned to a commit SHA.
