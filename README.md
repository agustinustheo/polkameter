<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-dark.png">
    <img src="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-light.png" alt="PolkaMeter" width="380">
  </picture>
</p>

# PolkaMeter

<p align="center">
  <b>Load and stress tests for Polkadot SDK chains, with verdicts and reports for CI.</b>
</p>

<p align="center">
  <a href="https://github.com/agustinustheo/polkameter/actions/workflows/ci.yml?query=branch%3Amain"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/agustinustheo/polkameter/ci.yml?branch=main&label=build&logo=githubactions&logoColor=white"></a>
  <a href="https://github.com/agustinustheo/polkameter/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/agustinustheo/polkameter?logo=github&color=E6007A"></a>
  <a href="https://crates.io/crates/polkameter"><img alt="crates.io: polkameter" src="https://img.shields.io/crates/v/polkameter?label=polkameter&logo=rust&color=E6007A"></a>
  <a href="https://crates.io/crates/polkameter-plugin-sdk"><img alt="crates.io: polkameter-plugin-sdk" src="https://img.shields.io/crates/v/polkameter-plugin-sdk?label=polkameter-plugin-sdk&logo=rust&color=E6007A"></a>
  <a href="https://docs.rs/polkameter-plugin-sdk"><img alt="docs.rs" src="https://img.shields.io/docsrs/polkameter-plugin-sdk?logo=docsdotrs"></a>
  <a href="https://github.com/agustinustheo/polkameter/blob/main/LICENSE"><img alt="License: Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <br>
  <img alt="Rust 1.93+" src="https://img.shields.io/badge/rust-1.93%2B-F74C00?logo=rust&logoColor=white">
  <img alt="Linux x86-64 | arm64" src="https://img.shields.io/badge/linux-x86--64%20%7C%20arm64-555?logo=linux&logoColor=white">
  <img alt="macOS arm64 | x86-64" src="https://img.shields.io/badge/macOS-arm64%20%7C%20x86--64-555?logo=apple&logoColor=white">
</p>

---

PolkaMeter loads a Polkadot SDK chain with signed transactions at rising rates and tells you where it stops keeping up. You choose the transaction rates in an XML plan (`*.polkameter.xml`). A plugin prepares transactions for your runtime.

A finished load test gives a verdict: `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. Every run also ends with an exit code.

## Highlights

- A GitHub Action runs a plan and fails the job on a `FAIL` or `INCONCLUSIVE` verdict.
- The [substrate plugin](https://agustinustheodorus.com/polkameter/substrate-plugin.html#limits) loads standard runtimes, so you write no plugin code.
- It raises the rate step by step until the chain breaks a limit, then checks how fast the chain recovers.

## Install

The install script downloads the CLI and the substrate plugin for Linux or macOS, checks the checksum and installs both in `~/.local/bin`:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

[Install](https://agustinustheodorus.com/polkameter/install.html) also covers `cargo binstall` and the release archives.

## Next steps

- [Getting started](https://agustinustheodorus.com/polkameter/getting-started.html) starts a dev chain and loads it until a limit fails.
- [Run in CI](https://agustinustheodorus.com/polkameter/ci.html) runs the same test on every pull request.
- [Use PolkaMeter with an AI assistant](https://agustinustheodorus.com/polkameter/ai-assistants.html) sets it up for you.
- [Writing a plugin](https://agustinustheodorus.com/polkameter/writing-a-plugin.html) covers runtimes the substrate plugin doesn't fit.
- [Troubleshooting](https://agustinustheodorus.com/polkameter/troubleshooting.html) helps when something broke.

To build and test PolkaMeter, see [`AGENTS.md`](https://github.com/agustinustheo/polkameter/blob/main/AGENTS.md#build-and-test).

## License

PolkaMeter is licensed under the [Apache License 2.0](https://github.com/agustinustheo/polkameter/blob/main/LICENSE).

<sub>Polkadot is a trademark of the Web3 Foundation. PolkaMeter is an independent project and is not endorsed by the Web3 Foundation.</sub>
