# Run in CI

The PolkaMeter action runs a plan in a GitHub Actions job. It installs the CLI, installs your plugins and credentials, and runs the plan. Then it uploads the run's reports and selected records. It supports Linux and macOS runners. For other CI systems, see [GitLab CI](#gitlab-ci) and [Other CI systems](#other-ci-systems).

## Test a dev chain on every pull request

This job starts the dev chain, runs the short CI plan with `fail-on: verdict`, and uploads the run's reports. It caches the downloads on the script's hash. Commit these files to your repository:

- the job as `.github/workflows/load-test.yml`
- `dev-chain.sh` at the repository root, from the same PolkaMeter version
- the CI plan as `plans/substrate-remarks-ci.polkameter.xml`

```sh
curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/v0.3.0/scripts/dev-chain.sh
mkdir -p plans
curl -fsSL -o plans/substrate-remarks-ci.polkameter.xml https://raw.githubusercontent.com/agustinustheo/polkameter/v0.3.0/examples/substrate-remarks-ci.polkameter.xml
```

Keep the action's `version` equal to the plan's substrate plugin version, `<plugin id="substrate" version="0.3.0">`. The plugin ships with the CLI, and PolkaMeter refuses a plugin of another version.

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
        run: bash dev-chain.sh start
      # To test your own runtime instead of the template, build its wasm first and set
      # POLKAMETER_DEV_RUNTIME to its path on the step above.
      - id: load
        # @v0 is the action; version pins the CLI and the substrate plugin it installs.
        uses: agustinustheo/polkameter@v0
        with:
          version: 0.3.0
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

`plugins: polkameter-substrate-plugin` names the plugin installed with the CLI. [Getting started](getting-started.md) runs the same chain by hand.

## Load your own node

Save this as `.github/workflows/load-test.yml`. Replace `<plan>`, `<plugin-binary>` and `<profile>` with your plan's path, your plugin executable and the plan's credential profile:

```yaml
name: load-test
on: workflow_dispatch

jobs:
  load-test:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v7
      - id: load
        uses: agustinustheo/polkameter@v0
        with:
          plan: <plan>
          plugins: plugins/<plugin-binary>
          fail-on: verdict
          credentials: <profile>=POLKAMETER_SETUP_SURI
        env:
          POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

`@v0` installs the latest 0.x release of the CLI and checks its checksum and build attestation. To fix the version, use a full tag such as `@v0.3.0`. A branch or commit SHA needs the `version` input.

Build your plugin in an earlier step with `cargo build --locked --release -p <plugin-package>` and pass `target/release/<plugin-binary>`.

Most jobs set only `plan`, `plugins`, `credentials`, `fail-on` and `version`. The action's outputs include `verdict`, `exit-code` and `run-dir`. [`action.yml`](https://github.com/agustinustheo/polkameter/blob/main/action.yml) describes every input and output.

## Secrets

Set secrets in the step's `env:` block. Put only profile and variable names in `credentials`. Inputs are not secret.

The `credentials` input maps a plan's [credential profile](cli.md#plugin) to an environment variable. Plugins get the value only in steps that use the credential. The files PolkaMeter writes and the job summary show `[redacted]` in place of the value. [Redaction](results.md#redaction) lists what is and is not covered.

Do not run plans with secrets on `pull_request_target` or for pull requests from forks.

## What fails the job

The job fails when `polkameter run` exits with a code other than `0`. [Exit codes](cli.md#exit-codes) lists the codes.

The `fail-on` input decides which [verdicts](reading-results.md#did-it-pass) also fail the job. The action passes it to [`polkameter run --fail-on`](cli.md#run), which has the same default.

| `fail-on` | `PASS` | `WARN` | `FAIL` | `INCONCLUSIVE` |
| --- | --- | --- | --- | --- |
| `verdict-or-inconclusive` (default) | passes | passes | fails | fails |
| `verdict` | passes | passes | fails | passes |
| `error` | passes | passes | passes | passes |

A failed run fails the job under every policy. A plan without a load test has no verdict. Smoke mode has stricter rules; see [`run`](cli.md#run).

A ramp that pushes the chain past its limit usually ends FAIL. To keep that job green and read the verdict instead, set `fail-on: error`. A later step can read the outputs even when the action failed:

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

The action adds the run's `summary.md` to the job summary. It uploads the run's reports and selected records as an artifact, even when the run failed. The `artifact-name` output gives the artifact's name. Files that plugins write into the run directory stay on the runner, because PolkaMeter does not redact them. If validation fails before the run starts, there is no artifact.

## GitHub test reports

The run directory contains `junit.xml`, a test report that CI systems can display. To show its cases in a GitHub check, give the job `checks: write` permission:

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

Use a Linux x86-64 runner. Add this job to `.gitlab-ci.yml`, and commit `dev-chain.sh` and the plan as for GitHub:

```yaml
load-test:
  image: debian:bookworm-slim
  variables:
    POLKAMETER_VERSION: "0.3.0"
    POLKAMETER_PLUGIN_REGISTRY: /tmp/polkameter-plugins.json
    # //Alice is funded on the dev chain. On a shared network, use a masked CI/CD variable.
    POLKAMETER_SUBSTRATE_SEED: //Alice
  cache:
    key:
      files: [dev-chain.sh]
    paths: [.polkameter/dev-chain/bin]
  before_script:
    - apt-get update && apt-get install -y --no-install-recommends ca-certificates curl
    - curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- "$POLKAMETER_VERSION"
    - export PATH="$HOME/.local/bin:$PATH"
  script:
    - bash dev-chain.sh start
    - polkameter plugin install "$HOME/.local/bin/polkameter-substrate-plugin"
    - polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
    - polkameter validate plans/substrate-remarks-ci.polkameter.xml
    - polkameter run plans/substrate-remarks-ci.polkameter.xml --output polkameter-runs --fail-on-verdict
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

Without the action, install the CLI with the install script on Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- <version>
export PATH="$HOME/.local/bin:$PATH"
```

Then run from the repository root. Replace `<plugin-binary>`, `<profile>`, `<ENV_VAR>` and `<plan>`:

```sh
polkameter plugin install target/release/<plugin-binary>
polkameter plugin credential <profile> <ENV_VAR>
polkameter validate <plan>
polkameter run <plan> --output polkameter-runs
```

Set `<ENV_VAR>` in the job's environment before `run`. Use a fresh output directory for each job. Keep the whole run directory, so [`polkameter report`](cli.md#report) can rebuild the reports later. Jenkins and other systems run the same commands.
