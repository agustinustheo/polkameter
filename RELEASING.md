# Releasing PolkaMeter

Every crate shares `[workspace.package].version` in the root `Cargo.toml`. Internal path dependencies carry the same version in `[workspace.dependencies]`. Two crates are published: `polkameter-plugin-sdk`, then `polkameter`. The example plugin and the substrate plugin have `publish = false`. The substrate plugin ships as an executable in the CLI archives and the container instead.

## Cut a release

1. Update the workspace version, every internal dependency version and the `version` in `.claude-plugin/plugin.json`. Run `cargo +1.93.0 check --workspace` so `Cargo.lock` follows. Retitle the `Unreleased` section of `CHANGELOG.md` with the version. Copy its notes into the GitHub release after publication.
2. Run the checks in [`AGENTS.md`](AGENTS.md#build-and-test), `python3 tests/distribution.py`, `shellcheck install.sh`, `actionlint`, and `cargo +1.93.0 publish --workspace --exclude polkameter-example-plugin --exclude polkameter-substrate-plugin --dry-run --locked`.
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

A prerelease runs every build and attestation and publishes a GitHub prerelease. It pushes only the container's full version tag, such as `:0.3.0-rc.1`. It never moves `:latest`, `:<major>`, `:<major>.<minor>` or the `v<major>` action tag. It never publishes to crates.io, because a published crate version is permanent. After the rehearsal, set the version to `0.3.0`, merge and tag.

## Recover from a failed release

Re-run the failed jobs in the original tag run (`gh run rerun <run-id> --failed`). The re-run reuses the builds of the jobs that passed. The crates.io job skips a crate whose version is already on crates.io, so a re-run after a partial publication publishes only the rest.

The GitHub release job refuses a release that is already published, because users may have pinned its checksums. A draft left by a failed run is finished: the job uploads every asset again and publishes it. To change a published release, publish a patch release. Never move a version tag to recover a release.

If the original run can no longer be re-run and the release is not published yet, dispatch the workflow on the release tag, and use the tag for both `--ref` and `tag`. Don't dispatch on `main`, or the attestations won't match the tag and the installers will refuse them:

```sh
gh workflow run release.yml --ref v0.3.0 -f tag=v0.3.0
```

## One-time setup

1. Create the GitHub environment `crates-io`. Allow version tags if you restrict deployment branches.
2. In each existing crate's crates.io settings, configure Trusted Publishing for owner `agustinustheo`, repository `polkameter`, workflow `release.yml` and environment `crates-io`. See the [crates.io Trusted Publishing documentation](https://crates.io/docs/trusted-publishing).
3. The workflow publishes only with Trusted Publishing. Publish a new crate's first version by hand, then configure Trusted Publishing for it.
4. Allow GitHub Actions to write repository contents and GHCR packages. Make the GHCR package public after its first publication, so CI jobs can pull it without signing in.

After a real release, check the downloads on all five platforms, `cargo binstall polkameter`, GHCR pulls on both architectures, `gh attestation verify` on one archive, and the moving major tag.
