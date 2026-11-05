# Run PolkaMeter in CI

The PolkaMeter action runs a plan in a GitHub Actions job. It builds the CLI from its own ref, installs your plugins and credentials, runs the plan and uploads the run directory. It works on Linux and macOS runners. For other systems, see [Other CI systems](#other-ci-systems).

## A minimal workflow

Save this as `.github/workflows/load-test.yml`. It builds your plugin, then runs the plan:

```yaml
name: load-test
on: workflow_dispatch

jobs:
  load-test:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v7
      - run: cargo build --release -p <plugin-crate>
      - id: load
        uses: agustinustheo/polkameter@main
        with:
          plan: plans/transfers.polkameter.xml
          plugins: target/release/<plugin-binary>
          fail-on: verdict
          credentials: previewnet-sudo=POLKAMETER_SETUP_SURI
        env:
          POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

Replace `main` with a release tag or a full commit SHA. The action builds the CLI from the ref you name, so the ref also sets the CLI version.

The action installs the toolchain named by its `toolchain` input, Rust 1.93.0 by default, and sets it as the job's default. Later steps that run `cargo` use it too.

## Inputs and outputs

| Input | Required | Default | Description |
| --- | --- | --- | --- |
| `plan` | yes | | Path to the `.polkameter.xml` plan, relative to the workspace. |
| `plugins` | no | `""` | Plugin executables to install, one per line, relative to the workspace. |
| `credentials` | no | `""` | Credential profiles to map to environment variables, one `PROFILE=ENV_VAR` per line. Set the secret variables on the calling step with `env:`; secrets are never inputs. |
| `topologies` | no | `""` | Zombienet topologies to register, one `ALIAS=path` per line. |
| `capabilities` | no | `""` | Capabilities the network provisioner supplies, one name per line. |
| `output` | no | `polkameter-runs` | Artifact root, relative to the workspace. The run creates one `run-*` directory inside it. |
| `fail-on` | no | `error` | `error` preserves the CLI defaults; `verdict` also fails on a `FAIL` load verdict. Other values are invalid. |
| `upload` | no | `"true"` | Upload the run directory as a workflow artifact. |
| `artifact-name` | no | `polkameter-run` | Name of the uploaded run artifact. |
| `toolchain` | no | `1.93.0` | Rust toolchain that builds PolkaMeter. It must be 1.93 or newer. |

| Output | Description |
| --- | --- |
| `run-dir` | The run directory, relative to the workspace. |
| `verdict` | `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`, read from `summary.json`. Empty if no verdict was written, including plans without a load test. |
| `exit-code` | The exit code of `polkameter run`. See [What fails the job](#what-fails-the-job). |

## Secrets

The `credentials` input maps a plan's credential profile to the name of an environment variable. The `env:` block on the action step sets that variable from a repository secret. Plugins receive the value only in steps that use the credential. The run directory and the job summary show `[redacted]` in place of the value.

Never put a secret in `with:`. Inputs are not secret.

## What fails the job

The action fails the job with the exit code of `polkameter run`:

| Exit code | Meaning | Job |
| --- | --- | --- |
| `0` | The run completed. | Passes |
| `1` | The run failed: a plugin error, a failed `core.assert-equal` step or a deadline. In smoke mode, also a monitor problem or a required check with no result. With `fail-on: verdict`, also a `FAIL` verdict. | Fails |
| `2` | The plan or the arguments are invalid. An invalid plan fails earlier, in the `validate` step. | Fails |
| `130` | The run was stopped with Ctrl-C (SIGINT). | Fails |

Set `fail-on: verdict` on the action step to fail the job when the load verdict is `FAIL`:

```yaml
      - id: load
        uses: agustinustheo/polkameter@main
        with:
          plan: plans/transfers.polkameter.xml
          fail-on: verdict
```

The action passes `--fail-on-verdict` to the CLI. It still uploads artifacts when the verdict fails. `WARN` and `INCONCLUSIVE` do not trigger this policy, and a plan without a load test has no verdict to fail on.

With the default `fail-on: error`, a stress-mode load run exits `0` even when its verdict is `FAIL`. A later step can inspect the `verdict` output even when the action failed:

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

- **Job summary.** The action appends the run's `summary.md` to the job summary. For a plan without a load test, it holds only a short status.
- **Artifact.** The whole run directory, uploaded as `polkameter-run`. Set `artifact-name` to change the name. The upload runs even when the run failed. If validation fails before the run starts, there is no run directory and no artifact.

## GitHub test reports

The run directory uploaded by the Action contains `junit.xml`. Add a test-report step after the action to show its cases in a GitHub check. Give the job `checks: write` permission:

```yaml
permissions:
  contents: read
  checks: write
```

Use the `load` step ID from the minimal workflow above:

```yaml
      - name: Publish test results
        if: always() && steps.load.outputs.run-dir != ''
        uses: dorny/test-reporter@a43b3a5f7366b97d083190328d2c652e1a8b6aa2 # v3.0.0
        with:
          name: PolkaMeter
          path: ${{ steps.load.outputs.run-dir }}/junit.xml
          reporter: java-junit
          fail-on-error: 'false'
          fail-on-empty: 'false'
```

The reporter displays failures while the PolkaMeter action keeps control of the job's exit policy. Set `fail-on-error: 'true'` to let the reporter also fail the job on JUnit failures. Publishing checks requires a token with write permission; use this example in workflows with that permission, such as a manual run in the repository.

## Other CI systems

Without the action, install the CLI in your job. It needs Rust 1.93 or newer:

```sh
cargo install --locked polkameter
```

`cargo install --locked --git https://github.com/agustinustheo/polkameter polkameter` installs the latest unreleased code.

Then run these from the repository root:

```sh
polkameter plugin install target/release/<plugin-binary>
polkameter plugin credential previewnet-sudo <ENV_VAR>
polkameter validate plans/transfers.polkameter.xml
polkameter run plans/transfers.polkameter.xml --output polkameter-runs --fail-on-verdict
```

The `plugin credential` line maps the profile to a variable name. Set that variable in the job's environment before `run`. Use a fresh output directory for each job. The exit codes are the same as in the table above; `--fail-on-verdict` exits `1` on `FAIL`.

Keep the whole run directory. `polkameter report <run-dir>` regenerates the checks from its files, and [Run directory](results.md) lists them.

## GitLab test reports

Install the CLI and your plugin as above, then write a copy to a fixed artifact path. Set any credential variables in GitLab's CI/CD settings:

```yaml
load-test:
  image: rust:1.93.0
  script:
    - cargo install --locked polkameter
    - cargo build --release -p <plugin-crate>
    - polkameter plugin install target/release/<plugin-binary>
    - polkameter plugin credential previewnet-sudo POLKAMETER_SETUP_SURI
    - polkameter run plans/transfers.polkameter.xml --output polkameter-runs --junit test-results/polkameter.xml --fail-on-verdict
  artifacts:
    when: always
    reports:
      junit: test-results/polkameter.xml
    paths:
      - polkameter-runs/
      - test-results/polkameter.xml
```

GitLab displays cases in the pipeline's Tests view and merge request test summary. `when: always` keeps reports from failed runs. A plan rejected before a run directory is created has no JUnit report.

For Jenkins, publish `polkameter-runs/**/junit.xml` with the `junit` pipeline step in an `always` post condition. The [results reference](results.md#junitxml) lists the case mapping, warning output and timings.
