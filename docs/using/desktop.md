# Desktop application

The desktop app is the interactive authoring and observability surface. It does not implement a separate runner: after you click **Arm and run**, the Rust backend uses the same scheduler, preflight, signing, execution, telemetry, and report code as the CLI.

## Core workflow

1. Define the target RPC and optional node Prometheus endpoint.
2. Add thread groups and samplers. Each group owns virtual users, concurrency, iterations, and an arrival model.
3. Store a named signer profile in the operating-system credential vault, then keep only its alias in the scenario.
4. Use **Preflight chain** to validate live metadata, argument encoding, and derived accounts.
5. Use **Arm and run** to submit. The button becomes **Stop** during an active run; stopping prevents new scheduled work and drains active watches within the configured deadline.
6. Open the generated run report to view the summary and SVG plots.

The first-launch guided tour (`?` in the top bar) explains each control. It can be replayed at any time.

## Plan editor

The left-hand test-plan tree contains the connection, each thread group and its samplers, assertions, and collectors. A group can contain setup, transaction, and teardown samplers:

- **Setup** samplers run once before load starts.
- **Transaction** samplers run for each virtual user and iteration under the selected arrival schedule.
- **Teardown** samplers run once after transaction work drains.

Groups run concurrently during the transaction phase, subject to each group's concurrency and the plan-wide `maxConcurrentSamples` ceiling. Setup and teardown are processed group by group.

## Save, load, and reset

- **Save scenario** writes a `.polkameter.json` document. It includes the profile name but redacts `baseSuri`.
- **Load scenario** rejects files that contain literal signer material.
- **Reset scenario** restores the sample 1,000-user transfer burst in the UI only; it does not delete saved files or vault entries.

## Local signer profiles

When you save a profile, the desktop application stores its SURI in the operating-system credential service under the Polkameter service name. A profile name must be no more than 128 characters and use only ASCII letters, digits, hyphens, underscores, or periods. Removing a profile deletes that vault credential.

Use a dedicated test key. The optional **Fund derived users** control is for local development only: it requires a loopback `ws://` endpoint and a development SURI beginning with `//`.

## Remote execution from the desktop

The editor can target a remote agent for the current session. The remote endpoint must be HTTPS or a loopback HTTP address exposed through an SSH tunnel. The token is session-only; the desktop sends a redacted scenario and the agent resolves its own signer. See [Remote runner agent](remote-agent.md).
