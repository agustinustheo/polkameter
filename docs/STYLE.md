# Writing the PolkaMeter docs

Write for someone who knows neither JMeter nor load testing. Lead with the result or action. Use US spelling. Give each page one purpose: tutorial, task guide, reference or explanation.

## Voice

Tutorials and explanations sound like Theo: direct, short and conversational. Address the reader as "you" and use contractions. Use "I" only for a decision Theo owns. Reference pages are terse and exact. Avoid marketing claims, decorative bold labels, meta introductions, stacked hedges and recap lists.

For definitions and instructions, give each sentence one topic. Aim for at most 20 words in instructions and 25 in descriptions. Use active voice. Define unfamiliar terms at first use, and use one term for one meaning: load test, rate step, flood transactions, probe transaction, baseline, finality, verdict, failure onset, max sustained rate, recovery. Write "rate step" for the load sense of "step".

Use ASD-STE100 wording for hard technical concepts only.

Keep verdict separate from exit code, and accepted, included and finalized separate. Missing evidence does not establish loss. p95 is inclusion latency. Quote flags, fields and printed labels exactly. Never invent output.

Show small, copyable commands. State the working directory and mark placeholders. Keep warnings beside the action they concern.

## One owner per fact

Each fact has one owner page. Other pages link to it in one sentence.

| Concept | Owner |
| --- | --- |
| What PolkaMeter is | `README.md` |
| Platforms, install methods, registering the substrate plugin, version pins | `install.md` |
| Dev chain: start, stop, own runtime, port; load only nodes you control | `getting-started.md` |
| Plugin install, registry, reinstall after a rebuild, credential profiles | `cli.md#plugin` |
| Smoke mode and `--fail-on` | `cli.md#run` |
| Signals and `--shutdown-grace` | `cli.md#stop-a-run` |
| Exit codes | `cli.md#exit-codes` |
| `init` flags; the plan `init` writes | `cli.md#init`; `substrate-plugin.md#generate-a-plan` |
| Plan elements, flood, probes, baseline, probe budget, run phases | `plans.md` |
| Verdict, check statuses, exit 0 is not a pass | `reading-results.md#did-it-pass` |
| Failure onset, max sustained rate, latency | `reading-results.md` |
| Loss statuses, state check, reorgs | `reading-results.md#did-transactions-get-lost` |
| Recovery | `reading-results.md#did-the-chain-recover` |
| Stop rules and failure classes | `reading-results.md#stop-rules` |
| Run directory files, redaction, outcome checks, `report` | `results.md` |
| Reconnects; node CPU and memory | `troubleshooting.md` entries `blocks: RPC connection lost` and `node: … no CPU or memory recorded` |
| The action, `fail-on`, CI jobs, CI secrets | `ci.md` |
| Plugin protocol, SDK | `plugins.md` (the 0.2 to 0.3 migration is in `CHANGELOG.md`) |
| Substrate operations, signers, limits | `substrate-plugin.md` |
| The skill, marketplace install, `llms.txt` | `ai-assistants.md` |
| Calls mode: call file, verdict, exit codes, limits | `calls-mode.md` |
| The calls mode against plain polkadot-api comparison | `examples/README.md`; `less-code.md` includes it, so don't copy its numbers |

The install script block also appears in `README.md` and `getting-started.md`. The skill's `SKILL.md` keeps its own workflow and links to Troubleshooting for fixes.

Use `--help` for flags, `action.yml` for action inputs and rustdoc for SDK symbols. Do not copy their tables.

## Troubleshooting

One entry per cause. The heading is the start of the message as the CLI prints it, without the `PolkaMeter:` prefix. When the message names its fix, keep the entry to a sentence or two. Add no entry when another page owns the cause.

## Releases

Document released behavior. Keep unreleased changes in CHANGELOG. A page has at most one pre-release note, marked `<!-- vX.Y.Z: remove after release -->`.

## Length

Replace or remove prose before you add more. A page that grows needs a cut elsewhere or a reason in the PR.
