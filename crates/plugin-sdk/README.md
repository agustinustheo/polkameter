# polkameter-plugin-sdk

The SDK for [PolkaMeter](https://github.com/agustinustheo/polkameter) plugins. A plugin is a separate executable that PolkaMeter starts and talks to over a JSON-line protocol. This crate is everything a plugin needs, behind features:

| Feature | Module | What it gives |
| --- | --- | --- |
| (default) | `polkameter_plugin_sdk` | The protocol: `Plugin`, `serve`, `Manifest`, `Operation`, `Schema`, `Context`, `Artifact`, `PreparedTx`, `StateCheckInput`. |
| `chain` | `polkameter_plugin_sdk::chain` | A Polkadot SDK node over RPC: `Client`, typed storage reads, events, signing and submission (subxt). |
| `observers` | `polkameter_plugin_sdk::observers` | Recording and judging a run: `RunDir`, the metric registry, the finalized-block `walker`, `ChainSeries`, `RunData` and check verdicts. Implies `chain`. |

```toml
[dependencies]
polkameter-plugin-sdk = "0.3"                                            # protocol only
polkameter-plugin-sdk = { version = "0.3", features = ["chain"] }        # + chain access
polkameter-plugin-sdk = { version = "0.3", features = ["observers"] }    # + recorders and checks
```

Without features the crate does not depend on subxt.

Version 0.3 replaces the 0.2 crates `polkameter-chain`, `polkameter-files`, `polkameter-checks` and `polkameter-monitors`: `polkameter_chain::` becomes `polkameter_plugin_sdk::chain::`, and the other three become `polkameter_plugin_sdk::observers::`. See [Migrating from 0.2](https://github.com/agustinustheo/polkameter/blob/main/docs/plugins.md#migrating-from-02).

The [plugin guide](https://github.com/agustinustheo/polkameter/blob/main/docs/plugins.md) has the protocol and the full API by feature, and [Writing a plugin](https://github.com/agustinustheo/polkameter/blob/main/docs/writing-a-plugin.md) walks through a first plugin.
