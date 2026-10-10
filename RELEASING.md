# Releasing Polkameter

Every crate in the workspace shares one version, set once in `[workspace.package]` in the root `Cargo.toml`. Internal path dependencies carry the same version in `[workspace.dependencies]`.

## Cut a release

1. Set `version` under `[workspace.package]` and the matching `version = "X.Y.Z"` in `[workspace.dependencies]` for each internal crate. Run `cargo check --workspace` so `Cargo.lock` follows.
2. Run the checks in the README, including `cargo publish --workspace --exclude polkameter-desktop --exclude polkameter-example-plugin --dry-run`.
3. Commit, merge to `main`, then tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.

The `Release` workflow then:

- builds the desktop bundles (AppImage and deb, dmg, NSIS) and the CLI binaries for each platform;
- publishes `polkameter`, `polkameter-engine`, `polkameter-load`, `polkameter-chain`, `polkameter-files`, `polkameter-checks`, `polkameter-monitors` and `polkameter-plugin-sdk` to crates.io, after checking that the tag is `v` plus the workspace version;
- creates the GitHub Release with the desktop bundles, CLI archives, SBOM and build attestations.

The desktop app (`polkameter-desktop`) and the example plugin are `publish = false` and are never sent to crates.io.

## One-time setup

1. Create an API token on crates.io with the publish-new and publish-update scopes.
2. In the GitHub repository, create an environment named `crates-io` and add the token as a secret named `CARGO_REGISTRY_TOKEN` in that environment (or as a repository secret with that name). Optionally restrict the environment to tag deployments by `v*`.
