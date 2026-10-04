# Retained migrated smoke report

Source: `target/pr37-live-results/run-1791020675891-18194`, the 360-transaction migrated People smoke run documented in `docs/pr37-validation.md` (2/4/6 tx/s, 30 seconds each). This is a replay fixture, not evidence of equivalence with upstream.

Only inputs consumed by the v2 CLI report and `measurement::check` are retained. `summary.json` contains the original expected check names/statuses and loss block; neither is regenerated to bless a test failure. The test creates the Markdown summary and derived samples/plots in a temporary directory.

Scrape text omits comments and metric families absent from the existing node-metric registry. All records, timestamps and registered metric samples are retained. `scrapes.jsonl.gz` uses deterministic gzip compression (mtime 0); the test expands it using a dev-only `flate2` dependency already present transitively in the lockfile. Other files are byte-for-byte copies, except that the local `artifact_dir` prefix in `events.jsonl` and `execution.json` is replaced with `/runs/`. The bundle is under 1 MB; no network, proofs, credentials or plugin installation is needed.

Original summary SHA-256: `9d0bba5d247635e8b4f0f062cb8b12c6ee309b62a332bbe4109e86407bfb2d83`.
