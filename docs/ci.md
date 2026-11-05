# Run PolkaMeter in CI

The PolkaMeter action runs a plan in a GitHub Actions job. It downloads a released CLI, installs your plugins and credentials, runs the plan and uploads the run directory. It works on Linux and macOS runners. For other systems, see [Other CI systems](#other-ci-systems).

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
        uses: agustinustheo/polkameter@v0
        with:
          plan: plans/transfers.polkameter.xml
          plugins: plugins/<plugin-binary>
          fail-on: verdict
          credentials: previewnet-sudo=POLKAMETER_SETUP_SURI
        env:
          POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

The moving `v0` tag exists from the next release on. Until then, pin `@v0.2.0`, which builds from source. Expect seconds with a release, minutes when building from source; plugin builds and the plan take additional time.

To pin the action, replace `v0` with a full release tag or commit SHA. With `install: auto`, full tags such as `v0.3.0` select that release. A moving major tag selects the release whose tag points at the action's commit, using the GitHub tags and releases API.

If needed, the action reads the CLI package version (or its inherited workspace version) and checks that release's tag against the action commit when it can determine it. Branches, commit SHAs and local `uses: ./` build from source. Missing releases or platform assets also fall back to source.

Set `version: <X.Y.Z>` to select a release explicitly, including from a branch or commit SHA. Set `install: release` to fail when it cannot download that release, or `install: source` to always build the CLI from the action's ref. The source build keeps its cache in `.polkameter-action-target` inside the action directory.

Downloads use the job's `GITHUB_TOKEN` and always check the archive's SHA-256 checksum. If `gh` is available, the action also verifies GitHub's build attestation, which identifies the repository that built the archive. Set `verify-attestation: "true"` to require verification (and `gh`), or `"false"` to skip it; failed checksum or requested attestation checks fail the job.

The action installs Rust only for a source build or when you set `plugin-crates`. It uses the `toolchain` input, Rust 1.93.0 by default, and sets that toolchain as the job's default. Later steps that run `cargo` use it too.

## Build plugins in the action

Use `plugin-crates` to build and install plugins from your checkout. Each line names a package directory or `Cargo.toml` path; add `:package` to select a package from a workspace. The action builds release executables and installs them before validating the plan.

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
| `install` | no | `auto` | `auto` downloads a release with a source fallback; `release` requires a matching asset; `source` always builds. |
| `version` | no | derived | Release version, `X.Y.Z` or `vX.Y.Z`. Empty uses the action's full release tag or resolves its moving major tag. Other refs build from source. |
| `verify-attestation` | no | enabled if `gh` exists | `"true"` requires `gh` and a valid build attestation; `"false"` skips verification. Checksums are always verified. |
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
| `junit` | Path to `<run-dir>/junit.xml` when present, or empty. |
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
        uses: agustinustheo/polkameter@v0
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
