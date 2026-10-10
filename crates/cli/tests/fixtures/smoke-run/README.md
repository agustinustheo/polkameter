# Recorded smoke run

A real 360-transaction smoke run (2, 4 and 6 tx/s for 30 seconds each) of a plugin workload on a local Previewnet fork, observing parachain 1502. The replay test feeds it to `polkameter report` and expects the recorded check names and statuses, and the recorded loss block, back.

Only the files the report reads are kept. `plugin-checks.json` holds the verdicts a plugin returned during the run; the report merges them after the built-in checks, as it does for a live run. Scrape text omits comments and metric families outside the node-metric registry; every record, timestamp and registered sample is kept. `scrapes.jsonl.gz` uses deterministic gzip compression (mtime 0), and the test expands it with the dev-only `flate2` dependency. The local `artifact_dir` prefix in `events.jsonl` and `execution.json` is replaced with `/runs/`. No network, credentials or plugin installation is needed.
