# Releases and maintenance

Releases are manually dispatched from `.github/workflows/release.yml`. The workflow requires a tag input such as `v0.1.0` and uses Rust 1.93.0.

## What a release builds

The matrix builds Tauri bundles on:

| Platform | Runner | Desktop bundles |
|---|---|---|
| Linux | `ubuntu-22.04` | AppImage, Debian package |
| macOS | `macos-14` | DMG |
| Windows | `windows-latest` | NSIS installer |

Each platform also builds the release `polkameter` CLI. The workflow uploads all bundles and CLI binaries, creates an SPDX JSON SBOM, attests build provenance for the release artifacts, and creates a GitHub release targeting the selected commit with generated notes.

## Release checklist

1. Ensure the intended commit has passing CI and docs build.
2. Verify version metadata and release notes scope.
3. Manually dispatch **Release** with the desired immutable tag.
4. Inspect the published assets, SBOM, and provenance attestation.
5. Test the CLI `--help`, desktop launch, scenario validation, and a non-destructive preflight from the released binaries.

## Maintaining the documentation site

The docs are source-controlled mdBook files under `docs/`, with configuration in `book.toml`. Pull requests build but do not deploy. Only a successful documentation workflow on `main` deploys. If the repository name or Pages domain changes, update `site-url` in `book.toml`, the README link, and this documentation.

The docs workflow pins its mdBook version and caches its installed binary. Bump that version intentionally, test `mdbook build` locally, and review the rendered output before merging.
