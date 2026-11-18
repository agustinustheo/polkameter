# Use PolkaMeter with an AI assistant

A skill is a set of instructions an AI assistant loads for a task. Install the PolkaMeter skill, then ask your assistant to create a load test for your chain. It installs PolkaMeter and picks how to load your chain: [calls mode](calls-mode.md) for your own pallets' calls, or a plan with the substrate plugin. Then it runs the test on a [dev chain](getting-started.md#2-start-a-dev-chain) and adds a CI job.

## Install the skill in Claude Code

The PolkaMeter repository is also a Claude Code plugin marketplace. A Claude Code plugin isn't a PolkaMeter plugin. In a Claude Code session, add it and install the plugin that holds the skill:

```text
/plugin marketplace add agustinustheo/polkameter
/plugin install polkameter@polkameter
```

Claude Code lists the skill as `polkameter:polkameter`. To get updates, run `/plugin marketplace update polkameter`.

## Copy the skill

The skill is the [`skills/polkameter`](https://github.com/agustinustheo/polkameter/tree/main/skills/polkameter) directory. To use it without the marketplace, run this from your project's root. It copies the skill into `.claude/skills/`:

```sh
git clone --depth 1 https://github.com/agustinustheo/polkameter /tmp/polkameter
mkdir -p .claude/skills
cp -R /tmp/polkameter/skills/polkameter .claude/skills/
```

Copy it to `~/.claude/skills/` to use it in every project.

## Ask for a load test

The assistant loads the skill when you ask for a load test, a stress test or PolkaMeter. For example:

```text
Stress-test the chain my node serves at ws://127.0.0.1:9944 with PolkaMeter, and add a CI job that runs a short check on every pull request.
```

To load a call of your own pallet, name it:

```text
Stress-test Payroll.batch_pay on my chain at ws://127.0.0.1:9944 with PolkaMeter calls mode, at 5 tx/s for a minute.
```

The skill tells the assistant to copy numbers from `summary.md`, keep secrets out of plans, and load only nodes you control.

## Point an assistant at the docs

[`llms.txt`](https://agustinustheodorus.com/polkameter/llms.txt) lists the documentation pages with a one-line summary each, in the [llms.txt](https://llmstxt.org/) format. Give its URL to an assistant without skill support.
