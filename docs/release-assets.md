# Release assets

For example, a Linux x86-64 CLI archive for v0.3.0 is named `polkameter-0.3.0-x86_64-unknown-linux-gnu.tar.gz`. Its checksum file is `polkameter-0.3.0-x86_64-unknown-linux-gnu.tar.gz.sha256`. These CLI assets are available from v0.3.0.

Select your target from [GitHub releases](https://github.com/agustinustheo/polkameter/releases), then download the archive and its `.sha256` file. Linux binaries require glibc 2.35 or newer, as provided by Ubuntu 22.04 and Debian 12; Alpine's musl libc needs the container instead. macOS binaries target macOS 11 or newer.

| System | Target triple | Format |
| --- | --- | --- |
| Linux x86-64 | `x86_64-unknown-linux-gnu` | `.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows x86-64 | `x86_64-pc-windows-msvc` | `.zip` |

The asset naming contract for downloaders, including the GitHub Action, is:

- Release tag: `v<version>`, where `<version>` has no leading `v`.
- Unix archive: `polkameter-<version>-<target>.tar.gz`.
- Windows archive: `polkameter-<version>-x86_64-pc-windows-msvc.zip`.
- Checksum: the complete archive filename followed by `.sha256`, containing `<sha256>  <archive-filename>` and a newline.
- Archive root: only `polkameter` (`polkameter.exe` on Windows), the [substrate plugin](substrate-plugin.md) `polkameter-substrate-plugin` (`polkameter-substrate-plugin.exe`), `LICENSE` and `README.md`.
- Download URL: `https://github.com/agustinustheo/polkameter/releases/download/v<version>/<archive-filename>`.

On Linux, verify the checksum with `sha256sum -c <archive-filename>.sha256`; on macOS use `shasum -a 256 -c <archive-filename>.sha256`. On Windows use PowerShell's `Get-FileHash <archive-filename> -Algorithm SHA256` and compare it with the checksum file before extracting. The archives have GitHub build provenance attestations; `gh attestation verify <archive-filename> --repo agustinustheo/polkameter --signer-workflow agustinustheo/polkameter/.github/workflows/release.yml --source-ref refs/tags/v<version>` verifies one. `gh` must be signed in to github.com (`gh auth login` or `GH_TOKEN`); it cannot verify attestations anonymously.

A release becomes visible only after every asset is uploaded and attested. Published assets are never replaced: if a release is ever repaired, assets that were already published keep their checksums.

Desktop assets use the separate name `polkameter-desktop-<version>-<platform>.tar.gz`. Download the CLI archives above for command-line use.

See the [installation trust model](install.md#choose-a-version-or-directory) and [release recovery rules](https://github.com/agustinustheo/polkameter/blob/main/RELEASING.md#resume-an-existing-release).
