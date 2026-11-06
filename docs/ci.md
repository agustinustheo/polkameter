# Run PolkaMeter in CI

<!-- v0.3.0: remove after release -->
> **Unreleased:** The latest release is v0.2.0. The moving `@v0` tag, release downloads, `junit.xml`, `--junit`, `install.sh` and the container are available from v0.3.0. Each section below says which version it needs.

The PolkaMeter action runs a plan in a GitHub Actions job. It installs the CLI, installs your plugins and credentials, runs the plan and uploads the run directory. From v0.3.0, the action downloads a released CLI. <!-- v0.3.0: remove after release -->`@v0.2.0` builds the CLI from source, which takes several minutes. The composite action supports Linux and macOS runners. From v0.3.0, Windows CLI archives are published separately; the composite action is not supported end to end on Windows. For other systems, see [Other CI systems](#other-ci-systems).

## A minimal workflow

Save this as `.github/workflows/load-test.yml`. It runs your plan with prebuilt plugin executables from your checkout. Replace `<plan>`, `<plugin-binary>` and `<profile>` with your plan's path, your plugin executable and the plan's credential profile:

<!-- v0.3.0: remove after release (change @v0.2.0 to @v0) -->
```yaml
name: load-test
on: workflow_dispatch

jobs:
  load-test:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v7
      - id: load
        uses: agustinustheo/polkameter@v0.2.0
        with:
          plan: <plan>
          plugins: plugins/<plugin-binary>
          fail-on: verdict
          credentials: <profile>=POLKAMETER_SETUP_SURI
        env:
          POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

<!-- v0.3.0: remove after release -->
The example uses `@v0.2.0`, which builds from source. The moving `@v0` tag, release-binary installation and the inputs below for installation and plugin builds are available from v0.3.0. Installation typically takes seconds when a release binary exists and several minutes when building from source; plugin builds and the plan take additional time.

After a successful stable release, the release workflow moves `v0` to the latest `0.x` release (`v1` for `1.x`, and so on). The ref you name also sets the CLI version. To keep the action fixed, use a full release tag or commit SHA. From v0.3.0, `install: auto` selects a full tag's release, such as `v0.3.0`. For a moving major tag such as `@v0`, it reads the CLI package version from the downloaded action package (inheriting the workspace version from `Cargo.toml` when needed). It requests that exact version's release without querying the moving tag again.

Branches, commit SHAs and local `uses: ./` build from source. Missing releases or platform assets also fall back to source. The action emits a warning with the fallback reason and records the installation method, version and reason in the job summary.

Set `version: "0.3.0"` (or `"v0.3.0"`) to select a release explicitly, including from a branch or commit SHA. Set `install: release` to fail when that release cannot be installed, or `install: source` to always build the CLI from the action's ref, ignoring `version`. Any source fallback builds the action's own version, even when a different release was requested.

To test a release candidate, use its full tag, such as `@v0.3.0-rc.1`, or set `version: "0.3.0-rc.1"`. A moving major tag such as `@v0` never selects a prerelease. The source build keeps its cache in `.polkameter-action-target` inside the action directory.

Downloads always check the archive's SHA-256 checksum. By default, `gh` must verify the archive's build attestation from `agustinustheo/polkameter/.github/workflows/release.yml`, bound to the selected `refs/tags/v<X.Y.Z>` source ref and the archive's digest. If `gh` is missing, `auto` emits a warning and builds from source; `release` fails. If verification runs and fails (including network or missing-attestation errors), both modes fail the job. A checksum mismatch also fails both modes; these failures do not fall back to source.

To explicitly accept checksum-only installation, set `verify-attestation: "false"`. The action warns that release provenance is not verified. Checksums remain mandatory.

`gh` cannot verify attestations anonymously, even for this public repository. On github.com runners, the action gives `gh` the job's `GITHUB_TOKEN` for the github.com host only; no extra permission is needed.

On GitHub Enterprise Server (GHES), releases and attestations still come from `api.github.com` and `github.com`. The action never sends the enterprise runner token to those hosts. Release lookups and downloads are anonymous.

On GHES, the public API limits anonymous requests to 60 per hour per IP address. Hitting that limit falls back to a source build in `auto` mode. On github.com runners, the action sends `GITHUB_TOKEN` for release requests.

For attestation verification on GHES, `gh` uses any stored github.com authentication from its own configuration. The action never supplies the GHES runner token to `gh`. If `gh` is not signed in to github.com, `auto` warns and builds from source, and `release` fails. Sign `gh` in to github.com on the runner, or set `verify-attestation: "false"` to accept checksum-only installation.

Runners need outbound access to github.com and its release storage. An unavailable download falls back in `auto`, while a failed attestation verification is fatal as described above.

The action installs Rust only for a source build or when you set `plugin-crates`. It uses the `toolchain` input, Rust 1.93.0 by default, and sets that toolchain as the job's default. Later steps that run `cargo` use it too.

## Build plugins in the action

Use `plugin-crates` to build and install plugins from your checkout. Each line names a package directory or `Cargo.toml` path; add `:package` to select a package from a workspace. The action builds release executables with `--locked` and installs them before validating the plan. Commit an up-to-date `Cargo.lock` for each package or its workspace; a missing or stale lockfile fails with Cargo diagnostics. To allow dependency resolution and lockfile changes explicitly, set `plugin-unlocked: "true"`; the action emits a warning.

This configuration requires v0.3.0 or newer (`@v0` from that release):

```yaml
      - uses: actions/checkout@v7
      - uses: agustinustheo/polkameter@v0
        with:
          plan: examples/plugin-workflow.polkameter.xml
          plugin-crates: |
            plugins/example:polkameter-example-plugin
          fail-on: verdict
```

You can combine `plugin-crates` with `plugins` for prebuilt executables. A release CLI still avoids compiling PolkaMeter when you build plugins.

## Inputs and outputs

| Input | Required | Default | Description |
| --- | --- | --- | --- |
| `plan` | yes | | Path to the `.polkameter.xml` plan, relative to the workspace. |
| `plugins` | no | `""` | Plugin executables to install, one per line, relative to the workspace. |
| `plugin-crates` | no | `""` | Package directories or `Cargo.toml` paths to build and install, one `path` or `path:package` per line. |
| `plugin-unlocked` | no | `"false"` | Opt in to plugin builds that may update `Cargo.lock`; default builds require an up-to-date lockfile. |
| `install` | no | `auto` | `auto` downloads a release with a source fallback; `release` requires a matching asset; `source` always builds. |
| `version` | no | derived | Release version, `X.Y.Z` or `vX.Y.Z`, optionally with a prerelease such as `-rc.1`. Empty uses the action's full release tag or its embedded package version for a moving major tag. Other refs build from source. |
| `verify-attestation` | no | `"true"` | Require `gh` verification of the release workflow, source tag and archive digest. `"false"` explicitly opts out with a warning. Checksums are always verified. |
| `credentials` | no | `""` | Credential profiles to map to environment variables, one `PROFILE=ENV_VAR` per line. Set the secret variables on the calling step with `env:`; secrets are never inputs. |
| `topologies` | no | `""` | Zombienet topologies to register, one `ALIAS=path` per line. |
| `capabilities` | no | `""` | Capabilities the network provisioner supplies, one name per line. |
| `output` | no | `polkameter-runs` | Artifact root, relative to the workspace. The run creates one `run-*` directory inside it. |
| `fail-on` | no | `verdict-or-inconclusive` | `verdict-or-inconclusive` also fails on a `FAIL` or `INCONCLUSIVE` load verdict; `verdict` also fails on `FAIL`; `error` fails only when the run fails. Other values are invalid. <!-- v0.3.0: remove after release -->v0.2.0 accepts only `error` (its default) and `verdict`. See [What fails the job](#what-fails-the-job). |
| `upload` | no | `"true"` | Upload the run's reports as a workflow artifact. See [What the job keeps](#what-the-job-keeps). |
| `upload-raw-plugin-files` | no | `"false"` | Also upload files that plugins wrote into the run directory. PolkaMeter does not redact them. |
| `artifact-name` | no | derived | Name of the uploaded artifact. Empty uses `polkameter-run-<job>-<attempt>-<hash>`, unique for each matrix job and each use of the action in a job. |
| `artifact-retention-days` | no | `""` | Days to keep the artifact, 1 to 90. Empty uses the repository setting. |
| `artifact-size-limit-mb` | no | `"500"` | Upload at most this many MiB of run records. `0` means no limit. Reports are always uploaded. |
| `toolchain` | no | `1.93.0` | Rust toolchain that builds PolkaMeter or `plugin-crates`. It must be 1.93 or newer. |

| Output | Description |
| --- | --- |
| `version` | Installed CLI version, without the `v` prefix. |
| `install-method` | `release` or `source`. |
| `junit` | Path to `<run-dir>/junit.xml` when present, or empty. <!-- v0.3.0: remove after release -->Available from v0.3.0; v0.2.0 does not write JUnit reports. |
| `run-dir` | The run directory, relative to the workspace. |
| `verdict` | `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`, read from `summary.json`. Empty if no verdict was written, including plans without a load test. |
| `exit-code` | The exit code of `polkameter run`. See [What fails the job](#what-fails-the-job). |
| `artifact-name` | Name of the run artifact, or empty if nothing was staged. Pass it to `actions/download-artifact` in a later job. |
| `artifact-path` | Directory on the runner holding the files staged for upload, or empty. |
| `artifact-id` | ID of the uploaded artifact, or empty if it was not uploaded. |

## Secrets

The `credentials` input maps a plan's credential profile to the name of an environment variable. The `env:` block on the action step sets that variable from a repository secret. Plugins receive the value only in steps that use the credential. The files PolkaMeter writes and the job summary show `[redacted]` in place of the value. Files that a plugin writes into the run directory itself are not redacted, so the action does not upload them unless you set `upload-raw-plugin-files: "true"`.

Redaction replaces the exact credential value. A plugin that prints an encoded or transformed form of a secret, such as base64, is not redacted. Do not run plans with secrets on `pull_request_target` or for pull requests from forks.

Never put a secret in `with:`. Inputs are not secret.

## What fails the job

The action fails the job with the exit code of `polkameter run`:

| Exit code | Meaning | Job |
| --- | --- | --- |
| `0` | The run completed. | Passes |
| `1` | The run failed: a plugin error, a failed `core.assert-equal` step or a deadline. In smoke mode, also a monitor problem or a required check with no result. With `fail-on: verdict`, also a `FAIL` verdict. With `fail-on: verdict-or-inconclusive`, the default, also a `FAIL` or `INCONCLUSIVE` verdict. | Fails |
| `2` | The plan or the arguments are invalid. An invalid plan fails earlier, in the `validate` step. | Fails |
| `130` | The run was stopped with SIGINT, for example when the job was cancelled. | Fails |
| `143` | The run was stopped with SIGTERM. | Fails |

A stopped run's `execution.json` says whether teardown finished (`teardown`). See [stop a run](cli.md#stop-a-run) for how long PolkaMeter needs between SIGTERM and SIGKILL.

The `fail-on` input decides which [load verdicts](reading-results.md#did-it-pass) also fail the job:

| `fail-on` | `PASS` | `WARN` | `FAIL` | `INCONCLUSIVE` | CLI flags |
| --- | --- | --- | --- | --- | --- |
| `verdict-or-inconclusive` (default from v0.3.0) | passes | passes | fails | fails | `--fail-on-verdict --fail-on-inconclusive` |
| `verdict` | passes | passes | fails | passes | `--fail-on-verdict` |
| `error` (default in v0.2.0) | passes | passes | passes | passes | none |

A failed run fails the job under every policy, and a plan without a load test has no verdict to fail on. A smoke-mode plan (`mode="smoke"`) fails the job on a monitor problem or a required check with no result under every policy, `error` included, so changing `fail-on` does not help there: fix the monitor or the missing result. The action uploads artifacts whatever the verdict.

When the verdict or smoke mode fails the job, the job summary ends with a "Why this job failed" section, and the job log has a matching error annotation. Both take the reason from `exit_reason` in [`execution.json`](results.md#executionjson):

- **`FAIL`:** the chain broke a stress criterion under load: the load stopped on a failure, a failure mode was found, a step broke a measure, or a check failed. A ramp plan that pushes the chain until it breaks usually ends `FAIL`. To keep such a job green and read the `verdict` output instead, set `fail-on: error`.
- **`INCONCLUSIVE`:** the run did not show whether the chain holds the load: the load ended before its rate cap, or a required check has no result. [Read the results](reading-results.md#what-to-do-about-inconclusive) says how to fix it. To let an `INCONCLUSIVE` run pass while you fix it, set `fail-on: verdict`.
- **Smoke mode:** the section names the monitor problem or the check with no result, and says that `fail-on` does not change it.

<!-- v0.3.0: remove after release (change @v0.2.0 to @v0) -->
```yaml
      - id: load
        uses: agustinustheo/polkameter@v0.2.0
        with:
          plan: <plan>
          fail-on: verdict
```

With `fail-on: error`, a stress-mode load run exits `0` even when its verdict is `FAIL`. A later step can inspect the `verdict` output even when the action failed:

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

- **Job summary.** From v0.3.0, it records the CLI installation method and reason. The action appends the run's `summary.md` to the job summary. For a plan without a load test, it holds only a short status.
- **Artifact.** The run's reports, uploaded even when the run failed. Read its name from the `artifact-name` output. If validation fails before the run starts, there is no run directory and no artifact.

From v0.3.0, the artifact holds only the files PolkaMeter writes; v0.2.0 uploads the whole run directory. The files are `execution.json`, `summary.md`, `summary.json`, `junit.xml`, `events.jsonl`, `samples.jtl`, the plots, the load-test records listed in [Run directory](results.md) and each plugin's redacted `stderr.log`. Other files in the run directory, such as a plugin's own diagnostics or its `series.jsonl`, stay on the runner. Set `upload-raw-plugin-files: "true"` to upload them too, after checking that your plugins never write secrets.

PolkaMeter redacts plugin logs and summaries when the run finishes, including a failed or stopped run. If the CLI was killed first, for example by the out-of-memory killer, those files still hold what the plugins wrote. The action then leaves `plugins/*/stderr.log`, `summary.md`, `summary.json` and `plugin-checks.json` out of the artifact and the job summary, with a notice. They stay in the run directory on the runner.

The action copies the files after `polkameter run` exits. A plugin process that is still running cannot change the uploaded copy, and the action never follows a symbolic link in the run directory.

If the records exceed `artifact-size-limit-mb`, the action keeps the reports and as many records as fit, in the order above, and warns. `polkameter-artifact-omitted.txt` in the artifact lists the omitted files. Set `artifact-retention-days` to keep the artifact for less time than the repository default.

On GHES, `actions/upload-artifact` v4 and newer are not supported, so the action skips the upload with a notice. Upload the staged files with an uploader your server supports:

```yaml
      - uses: actions/upload-artifact@v3
        if: always() && steps.load.outputs.artifact-path != ''
        with:
          name: ${{ steps.load.outputs.artifact-name }}
          path: ${{ steps.load.outputs.artifact-path }}
```

## GitHub test reports

<!-- v0.3.0: remove after release -->
> **Warning:** `junit.xml` is written from v0.3.0. With `@v0.2.0` the run directory has no `junit.xml`, so this step fails with no test results. Until v0.3.0 is released, pin the action to a commit SHA of this repository, such as `@58921b7d506f9987db68fc8084b2fb8f13649f94`. A commit SHA builds the CLI from source.

From v0.3.0, the run directory uploaded by the action contains `junit.xml`. Add a test-report step after the action to show its cases in a GitHub check. Give the job `checks: write` permission:

```yaml
permissions:
  contents: read
  checks: write
```

Use the `load` step ID from the minimal workflow above, with `uses: agustinustheo/polkameter@v0`:

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

The reporter displays failures while the PolkaMeter action keeps control of the job's exit policy. `fail-on-empty: 'true'` fails the step if the run directory has no `junit.xml`, so a missing report does not pass unnoticed. Set `fail-on-error: 'true'` to let the reporter also fail the job on JUnit failures. Publishing checks requires a token with write permission; use this example in workflows with that permission, such as a manual run in the repository.

## Other CI systems

Without the action, install the CLI on the runner. From v0.3.0, the install script downloads a prebuilt CLI on Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
```

For reproducible jobs, pass a version with `sh -s -- <version>` instead of `sh`. These recipes need a release containing CLI-only assets. See [Install](install.md) for supported systems and source installation. `gh` verifies the build attestation only when signed in to github.com; on GitHub Actions, set `GH_TOKEN: ${{ github.token }}` on the step. See [Install](install.md#use-the-install-script).

<!-- v0.3.0: remove after release -->
> **Warning:** The install script needs a v0.3.0 release. Until then, build the CLI with Cargo. A source build can take 10 minutes or more. `cargo install --locked polkameter` installs v0.2.0, which has no `--junit` and writes no `junit.xml`. For those, build from this repository instead: `cargo install --locked --git https://github.com/agustinustheo/polkameter --rev 58921b7d506f9987db68fc8084b2fb8f13649f94 polkameter`.

Configure subsequent shell steps to run from the repository root. Replace `<plugin-binary>`, `<profile>`, `<ENV_VAR>` and `<plan>` with your plugin executable, the plan's credential profile, the variable that holds the secret and your plan's path:

```yaml
- run: |
    polkameter plugin install target/release/<plugin-binary>
    polkameter plugin credential <profile> <ENV_VAR>
    polkameter validate <plan>
    polkameter run <plan> --output polkameter-runs --fail-on-verdict
```

The `plugin credential` line maps the profile to a variable name. Set that variable in the job's environment before `run`. Use a fresh output directory for each job. The exit codes are the same as in the table above; `--fail-on-verdict` exits `1` on `FAIL`. To also fail on `INCONCLUSIVE`, as the action does by default, add `--fail-on-inconclusive` (from v0.3.0).

Keep the whole run directory. `polkameter report <run-dir>` regenerates the checks from its files, and [Run directory](results.md) lists them.

## GitLab CI

<!-- v0.3.0: remove after release -->
> **Warning:** The container image and `--junit` are available from v0.3.0. Until then, use `rust:1.93.0-bookworm` as the image, add `cargo install --locked --git https://github.com/agustinustheo/polkameter --rev 58921b7d506f9987db68fc8084b2fb8f13649f94 polkameter` as the first script line, and remove `entrypoint`. The source build can take 10 minutes or more.

Use the published container as the job's `image:`. Build the plugin in an earlier Linux job and pass its executable as an artifact; it must match the runner's CPU architecture and the image's glibc 2.36. Use `rust:1.93.0-bookworm` as the plugin build job's image. Replace `<version>`, `<plugin-build-job>`, `<plugin-binary>`, `<profile>` and `<plan>` before saving this in `.gitlab-ci.yml`:

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

Set `POLKAMETER_SETUP_SURI` as a masked GitLab CI variable. The container runs as UID `10001` by default, so configure your runner to give that user write access to the checkout and output directory. Release container publication must complete before this job can pull the image.

`--junit` writes a copy of the run's JUnit report to a fixed path, and `reports: junit:` shows its cases in the pipeline's Tests view and the merge request test summary. `when: always` keeps reports from failed runs. A plan rejected before a run directory is created has no JUnit report.

When a job is cancelled or reaches its time limit, the CI system stops the container and may kill it before teardown finishes. To stop the run cleanly first, wrap it in `timeout`, which sends SIGINT and allows 45 seconds for teardown. Choose a duration shorter than the job's time limit:

```yaml
    - timeout --signal=INT --kill-after=45s 50m polkameter run <plan> --output polkameter-runs --fail-on-verdict
```

A run stopped this way still writes its results. `timeout` then exits `124`, which fails the job. When you start the container yourself, see [Stop a container run](install.md#stop-a-container-run).

## Jenkins

<!-- v0.3.0: remove after release -->
> **Warning:** The install script and `junit.xml` are available from v0.3.0. Until then, replace the install line with `cargo install --locked --git https://github.com/agustinustheo/polkameter --rev 58921b7d506f9987db68fc8084b2fb8f13649f94 polkameter` on an agent with Rust 1.93 or newer. The source build can take 10 minutes or more.

Save this as `Jenkinsfile` in your repository. It needs a Linux or macOS agent with `curl` and Rust 1.93 or newer. The first stage builds your plugin. Store the secret as a Jenkins "Secret text" credential, and replace `<plugin-package>`, `<credential-id>`, `<version>`, `<plugin-binary>`, `<profile>` and `<plan>`:

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

The `junit` step shows the cases in the build's test results, and `archiveArtifacts` keeps the run directory. Both run in `post { always }`, so failed runs keep their reports. With `allowEmptyResults: true`, a plan rejected before it writes a run directory does not also fail the `junit` step, so the build log shows the real error. A failed `polkameter run` fails the stage with its [exit code](#what-fails-the-job). The [results reference](results.md#junitxml) lists the case mapping, warning output and timings.
