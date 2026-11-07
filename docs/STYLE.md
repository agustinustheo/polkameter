# Writing the PolkaMeter docs

Write for someone who knows neither JMeter nor load testing. Lead with the result or action. Use US spelling. Give each page one purpose: tutorial, task guide, reference or explanation.

## Voice

Tutorials and explanations sound like Theo: direct, short and conversational. Address the reader as "you" and use contractions. Use "I" only for a decision Theo owns. Reference pages are terse and exact. Avoid marketing claims, decorative bold labels, meta introductions, stacked hedges and recap lists.

For definitions and instructions, give each sentence one topic. Aim for at most 20 words in instructions and 25 in descriptions. Use active voice. Define unfamiliar terms at first use, and use one term for one meaning: load test, rate step, flood transactions, probe transaction, baseline, finality, verdict, breaking point, max sustained rate, recovery.

Keep verdict separate from exit code, and accepted, included and finalized separate. Missing evidence does not establish loss. p95 is inclusion latency. Quote flags, fields and printed labels exactly. Never invent output.

Show small, copyable commands. State the working directory and mark placeholders. Keep warnings beside the action they concern. Do not document the desktop app.

## One owner per fact

Each fact has one owner page. Other pages link to it in one sentence.

| Concept | Owner |
| --- | --- |
| Smoke mode | `cli.md#run` |
| Redaction | `results.md#redaction` |
| Loss accounting and the state check | `results.md#loss-accounting` |
| Verdict | `reading-results.md#did-it-pass` |
| Reconnects and node sampling | `how-it-works.md#why-a-dropped-connection-is-not-a-chain-failure`, `how-it-works.md#node-cpu-and-memory` |
| Exit codes | `cli.md#exit-codes` |
| Credentials | `cli.md#plugin` |

Use `--help` for flags, `action.yml` for action inputs and rustdoc for SDK symbols. Do not copy their tables.

## Troubleshooting

One entry per cause. The heading is the message as the CLI prints it, without the `PolkaMeter:` prefix. Add no entry when the message names its fix or another page owns the cause.

## Releases

Document released behavior. Keep unreleased changes in CHANGELOG. A page has at most one pre-release note, marked `<!-- vX.Y.Z: remove after release -->`. A note about installing the CLI links to `install.md#before-v030-is-released`.

## Page budgets

Replace or remove prose before you add more. A page over budget needs a cut elsewhere or a reason in the PR.

| Page | Budget (words) |
| --- | ---: |
| `docs/README.md` | 190 |
| `docs/getting-started.md` | 550 |
| `docs/dev-chain.md` | 1,640 |
| `docs/writing-a-plugin.md` | 940 |
| `docs/install.md` | 640 |
| `docs/load-test.md` | 590 |
| `docs/reading-results.md` | 1,450 |
| `docs/ci.md` | 1,490 |
| `docs/remote-runs.md` | 790 |
| `docs/ai-assistants.md` | 330 |
| `docs/troubleshooting.md` | 2,170 |
| `docs/plans.md` | 3,050 |
| `docs/cli.md` | 2,050 |
| `docs/results.md` | 3,310 |
| `docs/plugins.md` | 1,700 |
| `docs/substrate-plugin.md` | 1,930 |
| `docs/how-it-works.md` | 1,160 |
| `docs/STYLE.md` | 580 |
| `docs/SUMMARY.md` | 90 |
| `docs/llms.txt` | 380 |
| `README.md` | 1,090 |
| `crates/plugin-sdk/README.md` | 240 |
| `skills/polkameter/SKILL.md` | 970 |
| `skills/polkameter/checklist.md` | 490 |
| `AGENTS.md` | 870 |
| `CHANGELOG.md` | 730 |
| `RELEASING.md` | 810 |
