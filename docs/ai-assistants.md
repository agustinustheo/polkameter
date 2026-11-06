# Use PolkaMeter with an AI assistant

PolkaMeter ships an Agent Skill and an `llms.txt` index, so an AI coding assistant can set up a load test for your chain: install PolkaMeter, pick the substrate plugin or start a plugin, write and validate a plan, run it on a dev chain and add the CI job.

## Install the skill

The skill is the directory [`skills/polkameter`](https://github.com/agustinustheo/polkameter/tree/main/skills/polkameter) in the PolkaMeter repository. It holds `SKILL.md`, a setup checklist, plan templates, CI templates and a plugin template. Copy the whole directory into your assistant's skills directory.

For Claude Code, copy it into your project, from your project's root:

```sh
git clone --depth 1 https://github.com/agustinustheo/polkameter /tmp/polkameter
mkdir -p .claude/skills
cp -R /tmp/polkameter/skills/polkameter .claude/skills/
ls .claude/skills/polkameter
```

```text
Cloning into '/tmp/polkameter'...
SKILL.md
checklist.md
templates
```

To use it in every project, copy it to `~/.claude/skills/` instead. For another assistant that reads Agent Skills, copy the directory into that assistant's skills directory.

The assistant loads the skill when you ask for a load test, a stress test or PolkaMeter. Then ask, for example:

```text
Stress-test the chain my node serves at ws://127.0.0.1:9944 with PolkaMeter, and add a CI job that runs a short check on every pull request.
```

The skill tells the assistant to read the verdict and numbers from the run's `summary.md` rather than estimate them, to keep secrets in credential profiles, and to load only nodes you control.

## Point an assistant at the docs

[`llms.txt`](https://agustinustheodorus.com/polkameter/llms.txt) lists the documentation pages with a one-line summary each, in the [llms.txt](https://llmstxt.org/) format. Give its URL to an assistant that has no skill support, or add it to a tool that indexes documentation for an LLM:

```text
https://agustinustheodorus.com/polkameter/llms.txt
```

## Work on PolkaMeter itself

[`AGENTS.md`](https://github.com/agustinustheo/polkameter/blob/main/AGENTS.md) at the repository root gives an assistant the build and test commands, the repository layout and the conventions for changes to PolkaMeter.
