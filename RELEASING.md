# Releasing PolkaMeter

Every crate shares `[workspace.package].version` in the root `Cargo.toml`. Internal path dependencies carry the same version in `[workspace.dependencies]`.

## Cut a release

1. Update the workspace version and every internal dependency version. Run `cargo +1.93.0 check --workspace` so `Cargo.lock` follows.
2. Run the checks in the README, `python3 tests/distribution.py`, `shellcheck install.sh`, `actionlint`, and `cargo +1.93.0 publish --workspace --exclude polkameter-desktop --exclude polkameter-example-plugin --dry-run --locked`.
3. Commit, merge to `main`, then tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.

The release workflow verifies that the tag matches the workspace version before publishing anything. It builds five CLI targets, packages only the executable, LICENSE and README, and uploads checksums and build attestations. Linux CLI builds use Ubuntu 22.04 userspace on x86-64 and ARM64, strip symbols, run `--version`, and check that no required GLIBC symbol exceeds 2.35. Desktop bundles are separate `polkameter-desktop-*` assets.

The [CLI asset naming contract](docs/release-assets.md) is consumed by binstall, the installer and download-based GitHub Actions. Preserve it when changing packaging. The workflow also publishes the multi-platform GHCR image with BuildKit provenance and an SBOM. Stable images get full version, major, minor and `latest` tags; prereleases get their full version only.

Crates publish one at a time in dependency order computed from `cargo metadata`. Packages with `publish = false` are excluded. The publisher checks the crates.io API with User-Agent `polkameter-release` and skips versions already published. HTTP 429 / rate-limit failures back off across six attempts, with up to 15 minutes of waiting per crate; other publishing failures stop immediately. Trusted Publishing credentials refresh during long waits.

After GitHub assets, the container and crates.io publication all succeed, stable releases force-update `v<major>` to the release commit. This enables `uses: agustinustheo/polkameter@v0`; use a full commit SHA for an immutable action version. Recovery runs for older versions do not move an existing major tag backwards. Tag protection must allow the workflow to update the moving major tag.

## Resume an existing release

Re-run failed jobs in the original tag-triggered run, or dispatch with the **tag itself as the workflow ref**, and the same tag input:

```sh
gh workflow run release.yml --ref v0.3.0 -f tag=v0.3.0
```

The tag must contain `.github/workflows/release.yml` with `workflow_dispatch` support. The workflow must also exist on the default branch for dispatch to be available. Do not move an immutable version tag to recover a release. A tag predating this workflow cannot acquire compatible attestations via a branch dispatch; publish a new version containing the workflow instead.

GitHub's [workflow_dispatch event](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_dispatch) records the selected branch or tag as `GITHUB_REF`. The [attestation provenance implementation](https://github.com/actions/toolkit/blob/main/packages/attest/src/provenance.ts) uses that ref, not `actions/checkout`'s `ref` or the `tag` input. Dispatching on `main` with `tag=v0.3.0` therefore records `refs/heads/main`, which fails the installers' `--source-ref refs/tags/v0.3.0` constraint. The prepare job rejects such runs before building or publishing. Keep the signer workflow path `.github/workflows/release.yml` for both installers.

Existing crate versions are skipped, GitHub assets are uploaded with `--clobber`, and release prerelease flags are reconciled on every run. Prereleases use `--prerelease --latest=false` and never update moving tags or container aliases. Stable container version tags are preserved on recovery; a mismatched version/revision label fails closed. Alias promotion is serialized repository-wide, compares each alias's current image version label numerically, and only moves forward. A recovery for `0.3.1` can update `:0.3` while preserving `:0` and `:latest` at `0.4.0`.

BuildKit generates container provenance and SBOM attestations using registry credentials; it does not sign them with GitHub OIDC. Only the crates.io and GitHub archive attestation jobs need `id-token: write`.

## One-time setup

1. Create the GitHub environment `crates-io`. Allow version tags if you restrict deployment branches.
2. In each existing crate's crates.io settings, configure Trusted Publishing for owner `agustinustheo`, repository `polkameter`, workflow `release.yml` and environment `crates-io`. See the [official auth action](https://github.com/rust-lang/crates-io-auth-action) and [crates.io Trusted Publishing documentation](https://crates.io/docs/trusted-publishing).
3. For new crates or environments without Trusted Publishing, add a crates.io API token with publish-new and publish-update scopes as `CARGO_REGISTRY_TOKEN` in the GitHub environment or repository secrets. The workflow tries `rust-lang/crates-io-auth-action` first, then uses this secret if token exchange is unavailable. For mixed coverage, each crate starts with Trusted Publishing and retries with the fallback secret only on HTTP 401/403. Logs name the credential mode without printing the token. An expired fallback token must be replaced before dispatching again.
4. Permit GitHub Actions to write repository contents and GHCR packages. Make the GHCR package public after its first publication so unauthenticated CI jobs can pull it.

A real release must verify all five platform downloads, binstall against the published crate metadata, GHCR pulls on both architectures, GitHub attestation verification, crates.io authentication and rate-limit handling, and the moving major tag. Local fixture tests do not publish to any registry.

## Historical secret-scan fixture

`.gitleaks.toml` covers three public Merkle-claim keys introduced in `984e61efb72cc7d7da8f11755a4bd105c46eca34`, in `plugins/scenarios/tests/vectors/claim.json`. The file was later removed; a full-history scan still encounters it. The allowlist requires the `generic-api-key` rule, the exact historical path, and one of the three exact values. It does not exempt the entire path or commit.

Verified with gitleaks 8.30.1 using `gitleaks git --log-opts=--all --redact`: the default configuration reports exactly three findings, all from that file and commit; the repository configuration reports zero. A control directory containing the historical fixture, the same keys at another path, and a different key at the historical path reports seven findings with defaults and four with the allowlist. Only the three intended findings disappear.
