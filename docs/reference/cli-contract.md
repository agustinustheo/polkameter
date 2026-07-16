# CLI exit codes and streams

## Exit status

| Code | Meaning |
|---:|---|
| `0` | Command succeeded; a run had no failed/timed-out samples. |
| `1` | Runtime or unexpected command error. |
| `2` | Invalid input, including scenario/load/argument errors. |
| `3` | Signer resolution or preflight failure. |
| `4` | Run completed but one or more samples failed or timed out. |
| `130` | `Ctrl-C` requested a graceful drain/stop. |

## Human format

`validate`, `preflight`, and `report` print their result to stdout. During `run`, progress and individual sample failures go to stderr so stdout can carry only the final artifact location and summary. Parse neither prose nor log ordering in automation.

## JSON format

`--format json` writes versioned JSON Lines to stdout. Events carry a `version` (currently `1`) and an `event` name. Validating the final event is the robust automation pattern; the repository's Zombienet smoke test requires a final `artifact-written` event.

Use the process exit status plus a validated artifact bundle as the authoritative result. JSON output is event data, not a replacement for the evidence files written by a run.
