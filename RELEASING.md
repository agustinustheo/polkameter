# Releasing PolkaMeter

Every crate shares `[workspace.package].version` in the root `Cargo.toml`. Internal path dependencies carry the same version in `[workspace.dependencies]`.

## Cut a release

1. Update the workspace version and every internal dependency version. Run `cargo +1.93.0 check --workspace` so `Cargo.lock` follows.
2. Run the checks in the README, `python3 tests/distribution.py`, `shellcheck install.sh`, `actionlint`, and `cargo +1.93.0 publish --workspace --exclude polkameter-desktop --exclude polkameter-example-plugin --dry-run --locked`.
3. Commit, merge to `main`, then tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.

The release workflow verifies that the tag matches the workspace version before publishing anything. It builds five CLI targets, packages only the executable, LICENSE and README, and uploads checksums and build attestations. Linux CLI builds use Ubuntu 22.04 userspace on x86-64 and ARM64, strip symbols, run `--version`, and check that no required GLIBC symbol exceeds 2.35. Desktop bundles are separate `polkameter-desktop-*` assets.

The [CLI asset naming contract](docs/install.md#download-release-archives) is consumed by binstall, the installer and download-based GitHub Actions. Preserve it when changing packaging. The workflow also publishes the multi-platform GHCR image with BuildKit provenance and an SBOM. Stable images get full version, major, minor and `latest` tags; prereleases get their full version only.

Crates publish one at a time in dependency order computed from `cargo metadata`. Packages with `publish = false` are excluded. The publisher checks the crates.io API with User-Agent `polkameter-release` and skips versions already published. HTTP 429 / rate-limit failures back off across six attempts, with up to 15 minutes of waiting per crate; other publishing failures stop immediately. Trusted Publishing credentials refresh during long waits.

After GitHub assets, the container and crates.io publication all succeed, stable releases force-update `v<major>` to the release commit. This enables `uses: agustinustheo/polkameter@v0`; use a full commit SHA for an immutable action version. Recovery runs for older versions do not move an existing major tag backwards. Tag protection must allow the workflow to update the moving major tag.

## Resume an existing release

Run the `Release` workflow with **Run workflow** on `main` and set `tag` to the existing version tag, such as `v0.2.0`. Dispatch uses release tooling from the selected workflow revision and source code from that tag, so fixes to the workflow can recover older tags. The version check still applies.

Existing crate versions are skipped, GitHub assets are uploaded with `--clobber`, and container tags are republished. Dispatching an older stable version also repoints its container aliases, including `latest`; dispatch the newest stable tag afterwards if needed. You can re-run failed jobs to avoid rebuilding successful jobs. Do not move an immutable version tag to recover a release.

## One-time setup

1. Create the GitHub environment `crates-io`. Allow both version tags and the branch used for recovery dispatches if you restrict deployment branches.
2. In each existing crate's crates.io settings, configure Trusted Publishing for owner `agustinustheo`, repository `polkameter`, workflow `release.yml` and environment `crates-io`. See the [official auth action](https://github.com/rust-lang/crates-io-auth-action) and [crates.io Trusted Publishing documentation](https://crates.io/docs/trusted-publishing).
3. For new crates or environments without Trusted Publishing, add a crates.io API token with publish-new and publish-update scopes as `CARGO_REGISTRY_TOKEN` in the GitHub environment or repository secrets. The workflow tries `rust-lang/crates-io-auth-action` first, then falls back to this secret if authentication is unavailable. An expired fallback token must be replaced before dispatching again.
4. Permit GitHub Actions to write repository contents and GHCR packages. Make the GHCR package public after its first publication so unauthenticated CI jobs can pull it.

A real release must verify all five platform downloads, binstall against the published crate metadata, GHCR pulls on both architectures, GitHub attestation verification, crates.io authentication and rate-limit handling, and the moving major tag. Local fixture tests do not publish to any registry.
