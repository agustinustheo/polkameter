# Remote runner agent

The remote agent lets a caller drive a stress host without transferring its signer secret. It runs the same Rust runner, writes artifacts locally on that host, and exposes a small authenticated HTTP API.

## Start an agent

```sh
POLKAMETER_AGENT_TOKEN='long-random-token' \
POLKAMETER_AGENT_SURI='//Alice' \
  polkameter agent serve \
  --bind 127.0.0.1:9901 \
  --output-root /var/lib/polkameter/runs
```

`--bind` defaults to `127.0.0.1:9901`, `--token-env` defaults to `POLKAMETER_AGENT_TOKEN`, and `--output-root` defaults to `target/polkameter-agent-runs`. The agent refuses an empty token or a non-loopback listening address.

Expose it through either an SSH tunnel or a TLS terminator. Clients accept `https://…` endpoints, or `http://` only on loopback addresses so an SSH forward can be used safely:

```sh
ssh -N -L 9901:127.0.0.1:9901 stress-host
```

## Run through the agent

```sh
POLKAMETER_REMOTE_TOKEN='long-random-token' \
  polkameter run scenario.polkameter.xml \
  --remote http://127.0.0.1:9901 \
  --remote-token-env POLKAMETER_REMOTE_TOKEN \
  --format json
```

Do not pass `--signer-env` with `--remote`; it is rejected. A `--signer-profile` only names the profile the agent should resolve. The remote run's artifacts remain under the agent's output root. The caller receives remote status and a validated report summary.

## Protocol and isolation

Every request uses bearer authentication and protocol version `1`. The CLI reads the XML plan and sends the agent an internal redacted document. Run IDs are constrained to ASCII letters, digits, hyphens, underscores, and periods, preventing path traversal through artifact paths. The received document must contain `[redacted]` as `baseSuri` and pass normal validation before an agent accepts it.

The available routes are documented in [Remote agent protocol](../reference/agent-protocol.md). The health endpoint is unauthenticated; all run operations require `Authorization: Bearer <token>`.
