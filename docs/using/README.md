# Using Polkameter

A Polkameter plan has four concerns:

1. **Connection:** a WebSocket RPC and optional Prometheus endpoint.
2. **Workload:** XML user groups, virtual users, an arrival model, and ordered setup/workflow/teardown calls.
3. **Credentials:** a named signer profile or a process environment variable—not a secret in the scenario.
4. **Evidence:** collectors that produce a portable artifact directory.

The desktop app makes these settings discoverable; it can load runtime metadata to turn a pallet call into labelled fields, while retaining a raw JSON editor for dynamically typed arguments. The CLI exposes the same plan for automation. Both ultimately pass the same native document to the Rust runner.
