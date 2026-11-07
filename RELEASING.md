# Releasing PolkaMeter

Every crate shares `[workspace.package].version` in the root `Cargo.toml`. Internal path dependencies carry the same version in `[workspace.dependencies]`. Two crates are published: `polkameter-plugin-sdk`, then `polkameter`. The desktop app, the example plugin and the substrate plugin have `publish = false`. The substrate plugin ships as an executable in the CLI archives and the container instead.

## Cut a release

1. Update the workspace version, every internal dependency version and the `version` in `.claude-plugin/plugin.json`. Run `cargo +1.93.0 check --workspace` so `Cargo.lock` follows. Retitle the `Unreleased` section of `CHANGELOG.md` with the version. Copy its notes into the GitHub release after publication.
2. Run the checks in [`AGENTS.md`](AGENTS.md#build-and-test), `python3 tests/distribution.py`, `shellcheck install.sh`, `actionlint`, and `cargo +1.93.0 publish --workspace --exclude polkameter-desktop --exclude polkameter-example-plugin --exclude polkameter-substrate-plugin --dry-run --locked`.
3. Commit, merge to `main`, then tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.
4. After the release is published, clean up the docs in one pull request. Remove each `vX.Y.Z: remove after release` marker with the text it marks (`grep -rn 'vX.Y.Z: remove after release' .`). Where a marker says so, replace `main` with the release tag. Then check `grep -rnE 'from v0\.3\.0|v0\.2\.0' docs skills README.md` for version notes the markers missed.

The release workflow checks that the tag matches the workspace version before it publishes anything. It builds five CLI targets and uploads the archives, checksums and build attestations. crates.io, the container aliases and the moving `v<major>` tag publish only after every build succeeds. The job graph is in `.github/workflows/release.yml`.

After a stable release, the workflow moves `v<major>` to the release commit, so `uses: agustinustheo/polkameter@v0` works. Tag protection must allow the workflow to update that tag.

## Asset names

Binstall, the installer and the GitHub Action download assets by name. Keep these names when you change packaging:

- Release tag: `v<version>`, where `<version>` has no leading `v`.
- Unix archive: `polkameter-<version>-<target>.tar.gz`.
- Windows archive: `polkameter-<version>-x86_64-pc-windows-msvc.zip`.
- Checksum: the complete archive filename followed by `.sha256`, containing `<sha256>  <archive-filename>` and a newline.
- Archive root: only `polkameter` (`polkameter.exe` on Windows), `polkameter-substrate-plugin` (`polkameter-substrate-plugin.exe`), `LICENSE` and `README.md`.
- Download URL: `https://github.com/agustinustheo/polkameter/releases/download/v<version>/<archive-filename>`.

## Rehearse with a release candidate

Rehearse with a prerelease tag before the first release that uses a changed workflow. The tag must match the workspace version. Set the version to, for example, `0.3.0-rc.1`, run `cargo +1.93.0 check --workspace`, commit and tag `v0.3.0-rc.1`.

A prerelease runs every build and attestation and publishes a GitHub prerelease. It pushes only the container's full version tag, such as `:0.3.0-rc.1`. It never moves `:latest`, `:<major>`, `:<major>.<minor>` or the `v<major>` action tag. It skips crates.io by default, because a published crate version is permanent.

To rehearse crates.io publication too, set this variable before you push the tag, and delete it afterwards:

```sh
gh variable set PUBLISH_PRERELEASE_CRATES --body true
```

A recovery dispatch can opt in with `-f prerelease-crates=true` instead. Cargo never selects a prerelease unless asked, so `0.3.0-rc.1` on crates.io doesn't affect users. After the rehearsal, set the version to `0.3.0`, merge and tag.

## Resume an existing release

Re-run the failed jobs in the original tag run. Or dispatch the workflow again, and use the release tag for both `--ref` and `tag`:

```sh
gh workflow run release.yml --ref v0.3.0 -f tag=v0.3.0
```

Don't dispatch on `main`, or the attestations won't match the tag and the installers will refuse them. Never move a version tag to recover a release.

A dispatch skips crate versions that already exist and uploads only missing assets. It never replaces a published asset, because users may have pinned its checksum. To correct a published asset, publish a patch release.

## One-time setup

1. Create the GitHub environment `crates-io`. Allow version tags if you restrict deployment branches.
2. In each existing crate's crates.io settings, configure Trusted Publishing for owner `agustinustheo`, repository `polkameter`, workflow `release.yml` and environment `crates-io`. See the [crates.io Trusted Publishing documentation](https://crates.io/docs/trusted-publishing).
3. For a new crate, add a crates.io API token with publish-new and publish-update scopes as `CARGO_REGISTRY_TOKEN` in the GitHub environment or repository secrets. The workflow uses it only when Trusted Publishing fails.
4. Allow GitHub Actions to write repository contents and GHCR packages. Make the GHCR package public after its first publication, so CI jobs can pull it without signing in.

After a real release, check the downloads on all five platforms, `cargo binstall polkameter`, GHCR pulls on both architectures, `gh attestation verify` on one archive, and the moving major tag.
