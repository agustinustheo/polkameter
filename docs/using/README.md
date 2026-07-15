# Using Polkameter

A Polkameter plan has four concerns:

1. **Connection:** a WebSocket RPC and optional Prometheus endpoint.
2. **Workload:** thread groups, virtual users, an arrival model, and ordered samplers.
3. **Credentials:** a named signer profile or a process environment variable—not a secret in the scenario.
4. **Evidence:** collectors that produce a portable artifact directory.

The desktop app makes these settings discoverable; the CLI exposes them for automation. Both ultimately pass the same native document to the Rust runner.
