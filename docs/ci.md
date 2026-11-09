# Run in CI

The PolkaMeter action runs a plan in a GitHub Actions job on a Linux or macOS runner. It installs the CLI and the substrate plugin, registers your plugins, maps your credential profiles to environment variables and runs the plan. Then it uploads the run's reports. For other CI systems, see [GitLab CI](#gitlab-ci) and [Other CI systems](#other-ci-systems).

## Test a dev chain on every pull request

This job starts the [dev chain](getting-started.md#2-start-a-dev-chain) and runs the short CI plan, a low-rate version of the Getting started plan. Commit these files to your repository:

- the job as `.github/workflows/load-test.yml`
- `dev-chain.sh` at the repository root
- the CI plan as `plans/substrate-remarks-ci.polkameter.xml`

```sh
curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/v0.4.2/scripts/dev-chain.sh
mkdir -p plans
curl -fsSL -o plans/substrate-remarks-ci.polkameter.xml https://raw.githubusercontent.com/agustinustheo/polkameter/v0.4.2/examples/substrate-remarks-ci.polkameter.xml
```

`@v0` always points at the newest 0.x action. The `version` input pins the CLI, so keep it on the same release as the files above. [Versions](install.md#versions) gives the rule.

```yaml
name: load-test
on:
  pull_request:
  workflow_dispatch:

permissions:
  contents: read

jobs:
  load-test:
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      # The node and runtime are pinned in dev-chain.sh; cache them on its hash.
      - uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: .polkameter/dev-chain/bin
          key: dev-chain-${{ runner.os }}-${{ hashFiles('dev-chain.sh') }}
      - name: Start the dev chain
        # For your own runtime, build its wasm in an earlier step and set
        # POLKAMETER_DEV_RUNTIME to its path in this step's env.
        run: bash dev-chain.sh start
      - id: load
        # @v0 is the action; version pins the CLI and the substrate plugin it installs.
        uses: agustinustheo/polkameter@v0
        with:
          version: 0.4.2
          plan: plans/substrate-remarks-ci.polkameter.xml
          plugins: polkameter-substrate-plugin
          credentials: substrate-dev=POLKAMETER_SUBSTRATE_SEED
          fail-on: verdict
        env:
          # //Alice is funded on the dev chain. On a shared network, use a secret instead.
          POLKAMETER_SUBSTRATE_SEED: //Alice
      - name: Stop the dev chain
        if: always()
        run: bash dev-chain.sh stop
```

`plugins: polkameter-substrate-plugin` names the plugin that the action installed with the CLI. `fail-on: verdict` fails the job on a FAIL verdict. [What fails the job](#what-fails-the-job) has the other policies.

## Load your own node

Save this as `.github/workflows/load-test.yml`. Replace `<plugin-package>` with your plugin's Cargo package, `<plugin-binary>` with its executable, `<plan>` with your plan's path and `<profile>` with the plan's credential profile:

```yaml
name: load-test
on: workflow_dispatch

jobs:
  load-test:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - name: Build the plugin
        run: cargo build --locked --release -p <plugin-package>
      - id: load
        uses: agustinustheo/polkameter@v0
        with:
          version: 0.4.2
          plan: <plan>
          plugins: target/release/<plugin-binary>
          credentials: <profile>=POLKAMETER_SETUP_SURI
          fail-on: verdict
        env:
          POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

With the substrate plugin, drop the build step and set `plugins: polkameter-substrate-plugin`. The action's outputs include `verdict`, `exit-code` and `run-dir`. [`action.yml`](https://github.com/agustinustheo/polkameter/blob/main/action.yml) describes every input and output.

## Secrets

Inputs show up in the workflow file and the logs. So `credentials` holds only names: profiles and variables. The secret itself goes in the step's `env:` block, from `secrets`.

The `credentials` input maps a plan's [credential profile](cli.md#plugin) to an environment variable. A plugin gets the value only in plan steps that reference the credential. PolkaMeter writes `[redacted]` in place of the value in its files and the job summary. [Redaction](results.md#redaction) lists what is and is not covered.

Don't run plans with secrets on `pull_request_target` or for pull requests from forks. Those runs would hand your secrets to code from the pull request.

## What fails the job

The job fails when `polkameter run` exits with a code other than `0`. [Exit codes](cli.md#exit-codes) lists the codes.

The `fail-on` input decides which [verdicts](reading-results.md#did-it-pass) also fail the job. The action passes it to [`polkameter run --fail-on`](cli.md#run), which has the same default.

| `fail-on` | `PASS` | `WARN` | `FAIL` | `INCONCLUSIVE` |
| --- | --- | --- | --- | --- |
| `verdict-or-inconclusive` (default) | passes | passes | fails | fails |
| `verdict` | passes | passes | fails | passes |
| `error` | passes | passes | passes | passes |

A failed run isn't a FAIL verdict. A run fails when PolkaMeter can't finish it, such as on a plan error or a failed setup step. That fails the job under every `fail-on` value. A plan without a load test has no verdict.

A ramp that pushes the chain past its limit usually ends FAIL. To keep that job green and still read the verdict, set `fail-on: error`. A later step can read the outputs even when the action failed:

```yaml
      - name: Show the load verdict
        if: always() && steps.load.outputs.run-dir != ''
        env:
          VERDICT: ${{ steps.load.outputs.verdict }}
          EXIT_CODE: ${{ steps.load.outputs.exit-code }}
        run: |
          echo "PolkaMeter exited $EXIT_CODE; verdict: ${VERDICT:-unavailable}"
```

## What the job keeps

The action adds the run's `summary.md` to the job summary. It uploads the run's reports and records as an artifact, even when the run failed. The `artifact-name` output gives the artifact's name. The upload leaves out files that plugins write into the run directory, because PolkaMeter doesn't redact them. If validation fails before the run starts, there is no artifact.

## GitHub test reports

The run directory contains [`junit.xml`](results.md#junitxml). To show its cases in a GitHub check, give the job `checks: write` permission:

```yaml
permissions:
  contents: read
  checks: write
```

Then add a report step after the `load` step:

```yaml
      - name: Publish test results
        if: always() && steps.load.outputs.run-dir != ''
        uses: dorny/test-reporter@a43b3a5f7366b97d083190328d2c652e1a8b6aa2 # v3.0.0
        with:
          name: PolkaMeter
          path: ${{ steps.load.outputs.run-dir }}/junit.xml
          reporter: java-junit
          fail-on-error: 'false'
          fail-on-empty: 'true'
```

The action still decides whether the job fails. `fail-on-empty: 'true'` fails the step if `junit.xml` is missing.

## GitLab CI

Use a Linux x86-64 runner, which the dev chain script needs. Add this job to `.gitlab-ci.yml`, and commit `dev-chain.sh` and the plan as for GitHub:

```yaml
load-test:
  image: debian:bookworm-slim
  variables:
    POLKAMETER_VERSION: "0.4.2"
    POLKAMETER_PLUGIN_REGISTRY: /tmp/polkameter-plugins.json
    # //Alice is funded on the dev chain. On a shared network, use a masked CI/CD variable.
    POLKAMETER_SUBSTRATE_SEED: //Alice
  cache:
    key:
      files: [dev-chain.sh]
    paths: [.polkameter/dev-chain/bin]
  before_script:
    - apt-get update && apt-get install -y --no-install-recommends ca-certificates curl procps
    - curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- "$POLKAMETER_VERSION"
    - export PATH="$HOME/.local/bin:$PATH"
  script:
    - bash dev-chain.sh start
    - polkameter plugin install "$HOME/.local/bin/polkameter-substrate-plugin"
    - polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
    - polkameter validate plans/substrate-remarks-ci.polkameter.xml
    - polkameter run plans/substrate-remarks-ci.polkameter.xml --output polkameter-runs --fail-on verdict
  after_script:
    - bash dev-chain.sh stop
  artifacts:
    when: always
    reports:
      junit: polkameter-runs/run-*/junit.xml
    paths:
      - polkameter-runs/
```

## Other CI systems

Without the action, install the CLI and the substrate plugin with the install script, then run from the repository root. For your own plugin, build it first and install `target/release/<plugin-binary>` instead. Replace `<profile>`, `<ENV_VAR>` and `<plan>`:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- 0.4.2
export PATH="$HOME/.local/bin:$PATH"
polkameter plugin install "$HOME/.local/bin/polkameter-substrate-plugin"
polkameter plugin credential <profile> <ENV_VAR>
polkameter validate <plan>
polkameter run <plan> --output polkameter-runs
```

Set `<ENV_VAR>` in the job's environment before `run`. Use a fresh output directory for each job. Keep the whole run directory, so [`polkameter report`](cli.md#report) can rebuild the reports later.
