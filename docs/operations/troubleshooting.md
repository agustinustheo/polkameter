# Troubleshooting

## Scenario validation fails

Run `polkameter validate scenario.polkameter.json --format json` and fix the reported `field` and message. Common causes are an empty group/sampler name, invalid RPC scheme, zero users, concurrency above users, a non-power-of-two mortality period, or a timeout under one second.

## Preflight cannot connect or cannot encode a call

Confirm the `chain.endpoint` is a reachable `ws://` or `wss://` URL, then use the runtime's exact pallet/call casing and argument shape. Preflight reads current metadata; a call that worked against one runtime may be absent or changed after an upgrade. Inspect `selectedCalls` in JSON output to isolate the failure.

## Signer profile cannot be found

For the desktop or `--signer-profile`, check the profile name and operating-system credential store. Profile names only allow ASCII letters/digits plus `-`, `_`, and `.`. In CI use `--signer-env NAME` and ensure `NAME` is present in that process. Never place the SURI in the scenario file.

## Derived users are not ready

The runner requires the derived virtual-user accounts to exist. On a fresh local dev chain, enable bounded local-only funding with a `//` development SURI and loopback endpoint. For every other chain, fund dedicated derived accounts through your normal process before running.

## Remote agent connection is rejected

The target must use HTTPS or loopback HTTP, and the bearer token cannot be empty. Start the agent on loopback only, use SSH forwarding or a TLS terminator, and confirm `GET /v1/health` returns ready. Do not use `--signer-env` with `--remote`.

## A run exits 4

The run finished but recorded failed or timed-out samples. This is distinct from malformed input or a failed preflight. Read `summary.md`, `samples.jtl`, and `events.jsonl`; use the response code/message and extrinsic/block hashes to determine whether the issue is a call failure, timeout, or assertion failure.

## `report` rejects a directory

The report command validates the complete artifact contract. Ensure the directory includes every required JSON/CSV/Markdown file and all six SVG plots, and that scenario/config documents retain a redacted signer source. Do not hand-edit an artifact unless you preserve the contract.

## Linux desktop build fails

Install the GTK/WebKit/Tauri build dependencies listed in [Install and build](../install.md). On CI or containers, use the same package set as `.github/workflows/ci.yml`.

## Documentation Pages deployment does not appear

Check that the workflow ran on `main`, not just a pull request, then open the deployment job logs. In GitHub repository Settings → Pages, select **GitHub Actions** as the source. The site is served at the project path `/polkameter/`, so links must account for that base URL.
