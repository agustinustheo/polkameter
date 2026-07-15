# Remote agent protocol

The protocol version is currently `1`. The agent listens on loopback and mounts these routes:

| Method | Path | Auth | Purpose |
|---|---|---:|---|
| `GET` | `/v1/health` | no | Returns protocol version and `ready` status. |
| `POST` | `/v1/preflight` | bearer | Preflights a redacted request. |
| `POST` | `/v1/runs` | bearer | Starts a run. |
| `GET` | `/v1/runs/{run_id}` | bearer | Reads current status. |
| `POST` | `/v1/runs/{run_id}/stop` | bearer | Requests a graceful stop. |
| `GET` | `/v1/runs/{run_id}/report` | bearer | Reads the validated dashboard report. |

`POST` request bodies contain `protocolVersion`, `runId`, and `document`. `runId` is limited to ASCII letters, digits, hyphens, underscores, and periods (maximum 128 characters). The scenario must be structurally valid and contain a `[redacted]` signer source.

An invalid/missing bearer token receives `401 Unauthorized`; an unknown run receives `404 Not Found`; request validation errors are `400 Bad Request`. The caller must use HTTPS unless the endpoint is a loopback HTTP SSH tunnel.
