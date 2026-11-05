# Run PolkaMeter in CI

The PolkaMeter action runs a plan in a GitHub Actions job. It downloads a released CLI, installs your plugins and credentials, runs the plan and uploads the run directory. The composite action supports Linux and macOS runners. From v0.3.0, Windows CLI archives are published separately; the composite action is not supported end to end on Windows. For other systems, see [Other CI systems](#other-ci-systems).

For jobs that only need the CLI, use the [prebuilt installer](install.md) or the [GitLab container](#gitlab-ci) to avoid compiling PolkaMeter. The action below currently builds the CLI; the release archive [naming contract](install.md#download-release-archives) also supports download-based actions.

## A minimal workflow

Save this as `.github/workflows/load-test.yml`. It runs your plan with prebuilt plugin executables from your checkout:

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
          plan: plans/transfers.polkameter.xml
          plugins: plugins/<plugin-binary>
          fail-on: verdict
          credentials: previewnet-sudo=POLKAMETER_SETUP_SURI
        env:
          POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

The example uses `@v0.2.0`, which builds from source. The moving `@v0` tag, release-binary installation and the inputs below for installation and plugin builds are available from v0.3.0. Installation typically takes seconds when a release binary exists and several minutes when building from source; plugin builds and the plan take additional time.

After a successful stable release, the release workflow moves `v0` to the latest `0.x` release (`v1` for `1.x`, and so on). The ref you name also sets the CLI version. To keep the action fixed, use a full release tag or commit SHA. From v0.3.0, `install: auto` selects a full tag's release, such as `v0.3.0`. For a moving major tag such as `@v0`, it reads the CLI package version from the downloaded action package (inheriting the workspace version from `Cargo.toml` when needed). It requests that exact version's release without querying the moving tag again.

Branches, commit SHAs and local `uses: ./` build from source. Missing releases or platform assets also fall back to source. The action emits a warning with the fallback reason and records the installation method, version and reason in the job summary.

Set `version: "0.3.0"` (or `"v0.3.0"`) to select a release explicitly, including from a branch or commit SHA. Set `install: release` to fail when that release cannot be installed, or `install: source` to always build the CLI from the action's ref, ignoring `version`. Any source fallback builds the action's own version, even when a different release was requested. The source build keeps its cache in `.polkameter-action-target` inside the action directory.

Downloads always check the archive's SHA-256 checksum. By default, `gh` must verify the archive's build attestation from `agustinustheo/polkameter/.github/workflows/release.yml`, bound to the selected `refs/tags/v<X.Y.Z>` source ref and the archive's digest. If `gh` is missing, `auto` emits a warning and builds from source; `release` fails. If verification runs and fails (including authentication, network or missing-attestation errors), both modes fail the job. A checksum mismatch also fails both modes; these failures do not fall back to source.

To explicitly accept checksum-only installation, set `verify-attestation: "false"`. The action warns that release provenance is not verified. Checksums remain mandatory.

On GitHub Enterprise Server (GHES), releases and attestations still come from `api.github.com` and `github.com`. The action never sends the enterprise runner token to those hosts. Release lookups and downloads are anonymous.

On GHES, the public API limits anonymous requests to 60 per hour per IP address. Hitting that limit falls back to a source build in `auto` mode. On github.com runners, the action sends `GITHUB_TOKEN` for release requests.

For attestation verification, `gh` uses any stored github.com authentication from its own configuration. The action never supplies the GHES runner token to `gh`.

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
| `version` | no | derived | Release version, `X.Y.Z` or `vX.Y.Z`. Empty uses the action's full release tag or its embedded package version for a moving major tag. Other refs build from source. |
| `verify-attestation` | no | `"true"` | Require `gh` verification of the release workflow, source tag and archive digest. `"false"` explicitly opts out with a warning. Checksums are always verified. |
| `credentials` | no | `""` | Credential profiles to map to environment variables, one `PROFILE=ENV_VAR` per line. Set the secret variables on the calling step with `env:`; secrets are never inputs. |
| `topologies` | no | `""` | Zombienet topologies to register, one `ALIAS=path` per line. |
| `capabilities` | no | `""` | Capabilities the network provisioner supplies, one name per line. |
| `output` | no | `polkameter-runs` | Artifact root, relative to the workspace. The run creates one `run-*` directory inside it. |
| `fail-on` | no | `error` | `error` preserves the CLI defaults; `verdict` also fails on a `FAIL` load verdict. Other values are invalid. |
| `upload` | no | `"true"` | Upload the run directory as a workflow artifact. |
| `artifact-name` | no | `polkameter-run` | Name of the uploaded run artifact. |
| `toolchain` | no | `1.93.0` | Rust toolchain that builds PolkaMeter or `plugin-crates`. It must be 1.93 or newer. |

| Output | Description |
| --- | --- |
| `version` | Installed CLI version, without the `v` prefix. |
| `install-method` | `release` or `source`. |
| `junit` | Path to `<run-dir>/junit.xml` when present, or empty. Available from v0.3.0; v0.2.0 does not write JUnit reports. |
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
        uses: agustinustheo/polkameter@v0.2.0
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

- **Job summary.** From v0.3.0, it records the CLI installation method and reason. The action appends the run's `summary.md` to the job summary. For a plan without a load test, it holds only a short status.
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

Without the action, install the prebuilt CLI on a Linux or macOS runner:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
```

For reproducible jobs, pass a version with `sh -s -- <version>` instead of `sh`. These recipes need a release containing CLI-only assets. See [Install](install.md) for supported systems and source installation.

Configure subsequent shell steps to run from the repository root, replacing the placeholders with your plugin executable and environment variable name:

```yaml
- run: |
    polkameter plugin install target/release/<plugin-binary>
    polkameter plugin credential previewnet-sudo <ENV_VAR>
    polkameter validate plans/transfers.polkameter.xml
    polkameter run plans/transfers.polkameter.xml --output polkameter-runs --fail-on-verdict
```

The `plugin credential` line maps the profile to a variable name. Set that variable in the job's environment before `run`. Use a fresh output directory for each job. The exit codes are the same as in the table above; `--fail-on-verdict` exits `1` on `FAIL`.

Keep the whole run directory. `polkameter report <run-dir>` regenerates the checks from its files, and [Run directory](results.md) lists them.

## GitLab CI

Use the published container as the job's `image:`. Build the plugin in an earlier Linux job and pass its executable as an artifact; it must match the runner's CPU architecture and the image's glibc 2.36. Use `rust:1.93.0-bookworm` as the plugin build job's image. Replace `<version>`, `<plugin-build-job>` and `<plugin-binary>` before saving this in `.gitlab-ci.yml`:

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
    - polkameter plugin credential previewnet-sudo POLKAMETER_SETUP_SURI
    - polkameter validate plans/transfers.polkameter.xml
    - polkameter run plans/transfers.polkameter.xml --output polkameter-runs --junit test-results/polkameter.xml --fail-on-verdict
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

For Jenkins, publish `polkameter-runs/**/junit.xml` with the `junit` pipeline step in an `always` post condition. The [results reference](results.md#junitxml) lists the case mapping, warning output and timings.
