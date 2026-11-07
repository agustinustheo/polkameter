# Run in CI

The PolkaMeter action runs a plan in a GitHub Actions job. It installs the CLI, installs your plugins and credentials, and runs the plan. Then it uploads the run's reports and selected records. It supports Linux and macOS runners. For other CI systems, see [Other CI systems](#other-ci-systems).

## A minimal workflow

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

<!-- v0.3.0: remove after release -->
Until v0.3.0 is released, `@v0` does not exist. Use `@259400a67647d9f662506e3b7469a0d565d7c676` instead, which builds the CLI from source in several minutes. Without the action, use the [source install](install.md#before-v030-is-released).

`@v0` installs the latest 0.x release of the CLI and checks its checksum and build attestation. To fix the version, use a full tag such as `@v0.3.0`. A branch or commit SHA builds the CLI from source. [`action.yml`](https://github.com/agustinustheo/polkameter/blob/main/action.yml) describes the `version`, `install` and `toolchain` inputs.

## Build plugins in the action

Use `plugin-crates` to build and install plugins from your checkout. List one plugin package per line. Use `path:package` to select a package in a workspace. The action builds with `--locked`, so commit an up-to-date `Cargo.lock`.

```yaml
      - uses: actions/checkout@v7
      - uses: agustinustheo/polkameter@v0
        with:
          plan: examples/plugin-workflow.polkameter.xml
          plugin-crates: |
            plugins/example:polkameter-example-plugin
          fail-on: verdict
```

You can combine `plugin-crates` with `plugins` for prebuilt executables.

## Inputs and outputs

These are the inputs most jobs set. [`action.yml`](https://github.com/agustinustheo/polkameter/blob/main/action.yml) describes all inputs and outputs.

| Input | Default | Meaning |
| --- | --- | --- |
| `plan` | required | Path to the plan, relative to the workspace |
| `plugins` | none | Plugin executables to install, one per line. A bare name, such as `polkameter-substrate-plugin`, is found on `PATH` |
| `plugin-crates` | none | Plugin packages to build and install, one `path` or `path:package` per line |
| `credentials` | none | Credential profiles to map, one `PROFILE=ENV_VAR` per line |
| `fail-on` | `verdict-or-inconclusive` | Which verdicts fail the job. See [What fails the job](#what-fails-the-job) |
| `version` | from the action's tag | The CLI release to install, such as `0.3.0` |

| Output | Meaning |
| --- | --- |
| `verdict` | `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. Empty if the run wrote no verdict |
| `exit-code` | The exit code of `polkameter run` |
| `run-dir` | The run directory, relative to the workspace |

## Secrets

Set secrets in the step's `env:` block. Put only profile and variable names in `credentials`. Inputs are not secret.

The `credentials` input maps a plan's [credential profile](cli.md#plugin) to an environment variable. Plugins get the value only in steps that use the credential. The files PolkaMeter writes and the job summary show `[redacted]` in place of the value. [Redaction](results.md#redaction) lists what is and is not covered.

Do not run plans with secrets on `pull_request_target` or for pull requests from forks.

## What fails the job

The job fails when `polkameter run` exits with a code other than `0`. [Exit codes](cli.md#exit-codes) lists the codes.

The `fail-on` input decides which [verdicts](reading-results.md#did-it-pass) also fail the job:

| `fail-on` | `PASS` | `WARN` | `FAIL` | `INCONCLUSIVE` | CLI flags |
| --- | --- | --- | --- | --- | --- |
| `verdict-or-inconclusive` (default) | passes | passes | fails | fails | `--fail-on-verdict --fail-on-inconclusive` |
| `verdict` | passes | passes | fails | passes | `--fail-on-verdict` |
| `error` | passes | passes | passes | passes | none |

A failed run fails the job under every policy. A plan without a load test has no verdict. In smoke mode, monitor problems and missing required check results fail the run under every policy; see [`run`](cli.md#run). Other verdict failures still follow `fail-on`.

When a verdict fails the job, the job summary ends with a "Why this job failed" section. A ramp that pushes the chain past its limit usually ends FAIL. To keep that job green and read the verdict instead, set `fail-on: error`. A later step can read the outputs even when the action failed:

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

## Other CI systems

Without the action, install the CLI with the install script on Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- <version>
export PATH="$HOME/.local/bin:$PATH"
```

On GitHub Actions, set `GH_TOKEN: ${{ github.token }}` on that step so `gh` can check build provenance. Then run from the repository root. Replace `<plugin-binary>`, `<profile>`, `<ENV_VAR>` and `<plan>`:

```yaml
- run: |
    polkameter plugin install target/release/<plugin-binary>
    polkameter plugin credential <profile> <ENV_VAR>
    polkameter validate <plan>
    polkameter run <plan> --output polkameter-runs --fail-on-verdict
```

Set `<ENV_VAR>` in the job's environment before `run`. Use a fresh output directory for each job. Add `--fail-on-inconclusive` to also fail on INCONCLUSIVE, as the action does by default. Keep the whole run directory, so [`polkameter report`](cli.md#report) can rebuild the reports later.

## GitLab CI

Use the published container as the job's `image:`. Build the plugin in an earlier Linux job with the `rust:1.93.0-bookworm` image, and pass the executable as an artifact. Replace `<version>`, `<plugin-build-job>`, `<plugin-binary>`, `<profile>` and `<plan>`:

```yaml
load-test:
  image:
    name: ghcr.io/agustinustheo/polkameter:<version>
    entrypoint: [""]
  needs:
    - job: <plugin-build-job>
      artifacts: true
  variables:
    POLKAMETER_PLUGIN_REGISTRY: /tmp/polkameter-plugins.json
  script:
    - polkameter plugin install target/release/<plugin-binary>
    - polkameter plugin credential <profile> POLKAMETER_SETUP_SURI
    - polkameter validate <plan>
    - polkameter run <plan> --output polkameter-runs --junit test-results/polkameter.xml --fail-on-verdict
  artifacts:
    when: always
    reports:
      junit: test-results/polkameter.xml
    paths:
      - polkameter-runs/
      - test-results/polkameter.xml
```

Set `POLKAMETER_SETUP_SURI` as a masked CI/CD variable. The container runs as UID `10001`, so give that user write access to the checkout. `--junit` copies the JUnit report to a fixed path for `reports: junit:`.

A cancelled job can kill the run before teardown ends. To stop it cleanly before the job's time limit, wrap the run in `timeout --signal=INT --kill-after=45s 50m`. The run still writes its results, and `timeout` exits `124`, which fails the job. [Stop a run](cli.md#stop-a-run) explains the signals.

[Stress-test a local dev chain](dev-chain.md#run-it-in-gitlab-ci) has a complete job that starts a dev chain.

## Jenkins

Save this as `Jenkinsfile`. It needs a Linux or macOS agent with `curl` and Rust 1.93 or newer. Store the secret as a "Secret text" credential. Replace `<plugin-package>`, `<credential-id>`, `<version>`, `<plugin-binary>`, `<profile>` and `<plan>`:

```groovy
pipeline {
  agent any
  environment {
    POLKAMETER_PLUGIN_REGISTRY = "${env.WORKSPACE}/polkameter-plugins.json"
    POLKAMETER_SETUP_SURI = credentials('<credential-id>')
  }
  stages {
    stage('Build plugin') {
      steps {
        sh 'cargo build --locked --release -p <plugin-package>'
      }
    }
    stage('Load test') {
      steps {
        sh '''
          curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- <version>
          export PATH="$HOME/.local/bin:$PATH"
          rm -rf polkameter-runs
          polkameter plugin install target/release/<plugin-binary>
          polkameter plugin credential <profile> POLKAMETER_SETUP_SURI
          polkameter validate <plan>
          polkameter run <plan> --output polkameter-runs --fail-on-verdict
        '''
      }
    }
  }
  post {
    always {
      junit testResults: 'polkameter-runs/**/junit.xml', allowEmptyResults: true
      archiveArtifacts artifacts: 'polkameter-runs/**', allowEmptyArchive: true
    }
  }
}
```

The `post { always }` block keeps the reports of failed runs too.
