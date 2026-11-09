# Use PolkaMeter with an AI assistant

Install the PolkaMeter skill, then ask your assistant to create a load test for your chain. The skill installs PolkaMeter and picks the substrate plugin or starts a plugin of your own. Then it writes a plan, runs it on a dev chain and adds a CI job.

## Install the skill in Claude Code

The PolkaMeter repository is a Claude Code plugin marketplace. In a Claude Code session, add it and install the plugin that holds the skill:

```text
/plugin marketplace add agustinustheo/polkameter
/plugin install polkameter@polkameter
```

Claude Code lists the skill as `polkameter:polkameter`. To get updates, run `/plugin marketplace update polkameter`.

## Copy the skill

The skill is the [`skills/polkameter`](https://github.com/agustinustheo/polkameter/tree/main/skills/polkameter) directory. To use it without the marketplace, copy it into your project's `.claude/skills/`, from your project's root:

```sh
git clone --depth 1 https://github.com/agustinustheo/polkameter /tmp/polkameter
mkdir -p .claude/skills
cp -R /tmp/polkameter/skills/polkameter .claude/skills/
```

Copy it to `~/.claude/skills/` to use it in every project, or into another assistant's skills directory.

## Ask for a load test

The assistant loads the skill when you ask for a load test, a stress test or PolkaMeter. For example:

```text
Stress-test the chain my node serves at ws://127.0.0.1:9944 with PolkaMeter, and add a CI job that runs a short check on every pull request.
```

The skill tells the assistant to copy numbers from `summary.md`, keep secrets out of plans and load only nodes you control.

## Point an assistant at the docs

[`llms.txt`](https://agustinustheodorus.com/polkameter/llms.txt) lists the documentation pages with a one-line summary each, in the [llms.txt](https://llmstxt.org/) format. Give its URL to an assistant without skill support.
