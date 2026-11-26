<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-dark.png">
    <img src="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-light.png" alt="PolkaMeter" width="380">
  </picture>
  
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

PolkaMeter sends signed transactions to a Polkadot SDK chain at rising rates and finds where it stops keeping up. You write the test as an XML plan (`*.polkameter.xml`), and a plugin builds the transactions for your runtime. Or you write only the calls, in [calls mode](#calls-mode-new-in-050).

A plan run ends with a [verdict](https://agustinustheodorus.com/polkameter/reading-results.html#did-it-pass): `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. The run also writes a summary, JSON results, JUnit and a record of every transaction. PASS means the chain met your limits at the rates you tested.

## Highlights

- Every transaction the load sends gets one outcome: finalized, still in the ready pool, refused, lost, unverified or unknown. PolkaMeter checks this against the finalized chain, so a transaction counts as lost only when the evidence shows it.
- PolkaMeter blames the chain only for the chain's faults. A short RPC disconnect or a pause of the load machine never counts as a stall or as latency, and a real stall still fails the run. [Whose fault is it](https://agustinustheodorus.com/polkameter/reading-results.html#whose-fault-is-it) lists the rules.
- A GitHub Action runs a plan and fails the job on a `FAIL` or `INCONCLUSIVE` verdict.
- The [substrate plugin](https://agustinustheodorus.com/polkameter/substrate-plugin.html#limits) funds accounts and sends remarks or transfers on standard runtimes, so you write no plugin code. For your own pallets, a plugin is any program that speaks JSON lines, in any language.
- It raises the rate one rate step at a time. A rate step sends transactions at one rate for a set time. PolkaMeter records the first rate step that fails a limit, such as latency, and the load goes on. Then it checks how fast the chain recovers.

## Calls mode (new in 0.5.0)

`polkameter calls` loads a chain with your own calls. You write no plan and no plugin. You write what to send with [polkadot-api](https://papi.how); PolkaMeter funds and signs for the test accounts, sends at a constant rate, and reconciles every transaction against the finalized chain. With Node.js 22.18 or later:

```sh
npm install polkadot-api && npx papi add dev -w ws://127.0.0.1:9944
echo 'export const call = ({ api, i }) => api.tx.TemplatePallet.do_something({ bn: i })' > calls.ts
polkameter calls --chain ws://127.0.0.1:9944 --seed //Alice --rate 5 --duration 60s --calls ./calls.ts --max-p95 30s
```

It exits 0 on PASS, 1 on FAIL, 4 when it cannot tell and 3 when setup failed. [Calls mode](docs/calls-mode.md) has the call file's interface, the flags and the limits. [examples/](examples/) has calls-mode tests of nine real chains next to the plain polkadot-api tests they replace: a median of 43 lines against 309.

## Install

The install script puts the CLI and the substrate plugin in `~/.local/bin` and checks their checksums:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

[Install](https://agustinustheodorus.com/polkameter/install.html) covers cargo, the release archives and supported platforms.

## Next steps

- [Your first load test](https://agustinustheodorus.com/polkameter/getting-started.html) starts a local dev chain and loads it past a limit.
- [Run in CI](https://agustinustheodorus.com/polkameter/ci.html) runs a short load test on every pull request.
- [Use PolkaMeter with an AI assistant](https://agustinustheodorus.com/polkameter/ai-assistants.html) sets it up for you.
- [Writing a plugin](https://agustinustheodorus.com/polkameter/writing-a-plugin.html) covers runtimes the substrate plugin doesn't fit.
- [Troubleshooting](https://agustinustheodorus.com/polkameter/troubleshooting.html) helps when something broke.

To build and test PolkaMeter, see [`AGENTS.md`](https://github.com/agustinustheo/polkameter/blob/main/AGENTS.md#build-and-test).

## License

PolkaMeter is licensed under the [Apache License 2.0](https://github.com/agustinustheo/polkameter/blob/main/LICENSE).

<sub>Polkadot is a trademark of the Web3 Foundation. PolkaMeter is an independent project and is not endorsed by the Web3 Foundation.</sub>
