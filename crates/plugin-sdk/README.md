# polkameter-plugin-sdk

The SDK for [PolkaMeter](https://github.com/agustinustheo/polkameter) plugins. PolkaMeter load- and stress-tests Polkadot SDK chains; a plugin prepares the transactions and checks for one runtime. A plugin is a separate executable that PolkaMeter starts and talks to over a JSON-line protocol on standard input and output. This crate is everything a plugin needs, behind features:

| Feature | Module | What it gives |
| --- | --- | --- |
| (default) | `polkameter_plugin_sdk` | The protocol: `Plugin`, `serve`, `Manifest`, `Operation`, `Schema`, `Context`, `Artifact`, `PreparedTx`, `StateCheckInput`. |
| `chain` | `polkameter_plugin_sdk::chain` | A Polkadot SDK node over RPC: `Client`, typed storage reads, events and submission (subxt). Signed transactions for any runtime from its metadata (`TxBuilder`: explicit nonce, mortality and tip, batches with consecutive nonces), test signers from secret URIs (`keypair`, `derive_range`), and account nonce and balances. |
| `observers` | `polkameter_plugin_sdk::observers` | Recording and judging a run: `RunDir`, the metric registry, the finalized-block `walker`, `ChainSeries`, `RunData` and check verdicts. Implies `chain`. |

<!-- v0.3.0: remove after release -->
> **Unreleased:** Version 0.3 is not on crates.io or docs.rs yet; both have 0.2.0. Until the release, the lines below fail with `failed to select a version`. Depend on a commit of the repository instead, with the features you need:
>
> ```toml
> polkameter-plugin-sdk = { git = "https://github.com/agustinustheo/polkameter", rev = "58921b7d506f9987db68fc8084b2fb8f13649f94", features = ["observers"] }
> ```
>
> To read the API documentation, run `cargo doc -p polkameter-plugin-sdk --all-features --open` in a checkout.

Add one of these lines to `[dependencies]`. For the protocol only:

```toml
polkameter-plugin-sdk = "0.3"
```

With chain access:

```toml
polkameter-plugin-sdk = { version = "0.3", features = ["chain"] }
```

With chain access, recorders and checks:

```toml
polkameter-plugin-sdk = { version = "0.3", features = ["observers"] }
```

Without features the crate does not depend on subxt.

`examples/hello.rs` is a complete plugin with one operation. The [API documentation](https://docs.rs/polkameter-plugin-sdk/0.3) starts with the same plugin. The [substrate plugin](https://github.com/agustinustheo/polkameter/tree/main/plugins/substrate), which signs remarks and transfers for any runtime with the `System` and `Balances` pallets, is built on the `chain` feature.

Version 0.3 replaces the 0.2 crates `polkameter-chain`, `polkameter-files`, `polkameter-checks` and `polkameter-monitors`: `polkameter_chain::` becomes `polkameter_plugin_sdk::chain::`, and the other three become `polkameter_plugin_sdk::observers::`. See [Migrating from 0.2](https://agustinustheodorus.com/polkameter/plugins.html#migrating-from-02).

The [plugin reference](https://agustinustheodorus.com/polkameter/plugins.html) has the protocol and the full API by feature, and [Writing a plugin](https://agustinustheodorus.com/polkameter/writing-a-plugin.html) walks through a first plugin.
