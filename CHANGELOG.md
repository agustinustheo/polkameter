# Changelog

User-visible changes in each release. The GitHub release notes list every merged pull request.

## Unreleased (0.3.0)

These notes cover the release pipeline, the GitHub Action, the installer and the container. Other 0.3.0 changes still need entries here.

### GitHub Action

Moving `@v0` to 0.3.0 changes these defaults for existing workflows:

- **Smaller artifact by default.** The run artifact holds only the files PolkaMeter writes: reports, records and each plugin's redacted `stderr.log`. Files that plugins write into the run directory are no longer uploaded. Set `upload-raw-plugin-files: "true"` to upload them.
- **New default artifact name.** The name was `polkameter-run`. It is now `polkameter-run-<job>-<attempt>-<hash>`, unique for each matrix job and each use of the action. Read it from the new `artifact-name` output, or set `artifact-name` to keep a fixed name.
- **Unfinished runs.** If the CLI is killed before the run finishes, plugin logs and summaries are not redacted. The action leaves them out of the artifact and the job summary.
- **Size limit.** At most 500 MiB of run records are uploaded (`artifact-size-limit-mb`). Reports are always uploaded.
- **GitHub Enterprise Server.** The action no longer fails when it uploads the artifact: it skips the upload with a notice. Upload the `artifact-path` output with an uploader your server supports.

Also new:

- `artifact-retention-days` input, and `artifact-name`, `artifact-path` and `artifact-id` outputs.
- Release candidates install from their release: use `@v0.3.0-rc.1` or `version: "0.3.0-rc.1"`. Moving tags never select a prerelease.
- On GHES, if `gh` is not signed in to github.com, `install: auto` builds from source instead of failing; `install: release` fails with guidance.
- Every action the composite action uses is pinned to a commit SHA, so organisations that require SHA-pinned actions can use it.

### Installer

- `install.sh` verifies the build attestation only when `gh` is signed in to github.com. If `gh` is installed but not signed in, as on GitHub-hosted runners, it installs after the checksum check and warns. `POLKAMETER_VERIFY_ATTESTATION=1` still requires verification.
- Failed verification messages mention expired or invalid `gh` credentials.

### Container

- The image runs `polkameter` under `tini`, and `docker stop` sends SIGINT, so a stopped run tears down and writes its results. Allow 45 seconds: `docker stop --timeout 45`.

### Releases

- Releases are published only after every asset is uploaded and attested. Published assets are never replaced.
- crates.io publication waits for the GitHub release and the container image.
