# Writing the PolkaMeter docs

Write for someone who knows neither JMeter nor load testing. Lead with the result or action. Use US spelling. Give each page one purpose: tutorial, task guide, reference or explanation.

## Voice

Tutorials and explanations sound like Theo: direct, short and conversational. Address the reader as "you" and use contractions. Use "I" only for a decision Theo owns. Reference pages are terse and exact. Avoid marketing claims, decorative bold labels, meta introductions, stacked hedges and recap lists.

For definitions and instructions, give each sentence one topic. Aim for at most 20 words in instructions and 25 in descriptions. Use active voice. Define unfamiliar terms at first use, and use one term for one meaning: load test, rate step, flood transactions, probe transaction, baseline, finality, verdict, breaking point, max sustained rate, recovery.

Keep verdict separate from exit code, and accepted, included and finalized separate. Missing evidence does not establish loss. p95 is inclusion latency. Quote flags, fields and printed labels exactly. Never invent output.

Show small, copyable commands. State the working directory and mark placeholders. Keep warnings beside the action they concern.

## One owner per fact

Each fact has one owner page. Other pages link to it in one sentence.

| Concept | Owner |
| --- | --- |
| What PolkaMeter is | `README.md` |
| Install methods, and which install the substrate plugin | `install.md` |
| Dev chain: start, stop, own runtime, port; load only nodes you control | `getting-started.md` |
| Plugin install, registry, reinstall after a rebuild, credential profiles | `cli.md#plugin` |
| Smoke mode and `--fail-on` | `cli.md#run` |
| Signals and `--shutdown-grace` | `cli.md#stop-a-run` |
| Exit codes | `cli.md#exit-codes` |
| `init` flags; the plan `init` writes | `cli.md#init`; `substrate-plugin.md#generate-a-plan` |
| Plan elements, flood, probes, baseline, probe budget, run phases | `plans.md` |
| Verdict, check statuses, exit 0 is not a pass | `reading-results.md#did-it-pass` |
| Breaking point, max sustained rate, latency | `reading-results.md` |
| Loss statuses, state check, reorgs | `reading-results.md#did-transactions-get-lost` |
| Recovery | `reading-results.md#did-the-chain-recover` |
| Stop rules and failure classes | `reading-results.md#stop-rules` |
| Run directory files, redaction, outcome checks, `report` | `results.md` |
| Reconnects; node CPU and memory | `troubleshooting.md` entries `blocks: RPC connection lost` and `node: … no CPU or memory recorded` |
| The action, `fail-on`, version pins, CI jobs, CI secrets | `ci.md` |
| Plugin protocol, SDK, 0.2 to 0.3 migration | `plugins.md` |
| Substrate operations, signers, limits | `substrate-plugin.md` |
| The skill, marketplace install, `llms.txt` | `ai-assistants.md` |

The install script block also appears in `README.md` and `getting-started.md`. The skill's `SKILL.md` keeps its own workflow and fix table.

Use `--help` for flags, `action.yml` for action inputs and rustdoc for SDK symbols. Do not copy their tables.

## Troubleshooting

One entry per cause. The heading is the message as the CLI prints it, without the `PolkaMeter:` prefix. Add no entry when the message names its fix or another page owns the cause.

## Releases

Document released behavior. Keep unreleased changes in CHANGELOG. A page has at most one pre-release note, marked `<!-- vX.Y.Z: remove after release -->`.

## Page budgets

Replace or remove prose before you add more. A page over budget needs a cut elsewhere or a reason in the PR.

| Page | Budget (words) |
| --- | ---: |
| `docs/README.md` | 110 |
| `docs/getting-started.md` | 1,000 |
| `docs/writing-a-plugin.md` | 900 |
| `docs/install.md` | 380 |
| `docs/ci.md` | 1,150 |
| `docs/reading-results.md` | 1,700 |
| `docs/ai-assistants.md` | 280 |
| `docs/troubleshooting.md` | 1,800 |
| `docs/plans.md` | 2,550 |
| `docs/cli.md` | 1,150 |
| `docs/results.md` | 1,900 |
| `docs/plugins.md` | 1,300 |
| `docs/substrate-plugin.md` | 1,600 |
| `docs/STYLE.md` | 700 |
| `docs/SUMMARY.md` | 60 |
| `docs/llms.txt` | 230 |
| `README.md` | 380 |
| `crates/plugin-sdk/README.md` | 110 |
| `skills/polkameter/SKILL.md` | 900 |
| `AGENTS.md` | 600 |
| `CHANGELOG.md` | 880 |
| `RELEASING.md` | 620 |
