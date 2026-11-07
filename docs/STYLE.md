# Writing the PolkaMeter docs

Write for someone who knows neither JMeter nor load testing. Lead with the result or action. Use US spelling.

Choose one purpose: tutorial, task guide, reference or explanation. Keep one topic per paragraph. Link to background when a task needs it.

## Voice

Tutorials and explanations should sound like Theo: direct, short and conversational. Address the reader as "you." Use contractions. Use "I" only for an explanation or decision Theo owns.

"Just," "please" and "let's" are allowed when useful in tutorials. Do not use them to dismiss difficulty. Avoid marketing claims, decorative bold labels, meta introductions, stacked hedges and recap lists.

Reference pages should be terse and exact.

## Hard concepts

Use Simplified Technical English principles for technical definitions and instructions:

- Give each sentence one topic.
- Aim for at most 20 words in instructions and 25 in descriptions.
- Use active voice and imperative instructions.
- Keep articles. Avoid noun clusters and unnecessary phrasal verbs.
- Use one term for one meaning. Define unfamiliar terms at first use.

Use these terms consistently: load test, stress test, rate step, ramp, flood transactions, probe transaction, baseline, finality, verdict, breaking point, max sustained rate and recovery.

Keep verdict separate from exit code. Keep accepted, included and finalized separate. Preserve loss statuses: `finalized`, `in_pool`, `refused`, `lost`, `unverified`, `unknown`. Missing evidence does not establish loss. Define p95 as inclusion latency, not an average or finality time. Explain the extra failure rules of smoke mode.

Use credential profile for the environment-variable mapping. State redaction coverage and exclusions. Explain that plugins are trusted executables and exchange one JSON message per line.

Preserve exact flags, fields and printed labels when quoting output.

## Keep it small

Each fact has one owner page. Link instead of repeating it. Definitions replace jargon; they do not require new sections.

Use `--help` for flags and rustdoc for SDK symbols. Do not duplicate their tables. Use tables only for comparisons or genuine reference data.

Show small, copyable commands. State the working directory and mark placeholders. Show real output only where it helps the reader check a result. Never invent output.

Add troubleshooting entries only for problems users encounter whose messages do not already explain the fix. Use the message as the CLI prints it, without the `PolkaMeter:` prefix, as the heading.

Document released behavior. Keep unreleased changes in CHANGELOG. Preserve the currently working install path until release cleanup, and mark each pre-release note with `<!-- vX.Y.Z: remove after release -->`.

Keep warnings beside the relevant action. Do not document the desktop app. Follow the agreed page budgets; replace or remove prose before adding more.
