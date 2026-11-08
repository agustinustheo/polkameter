# polkameter-plugin-sdk

The SDK for [PolkaMeter](https://github.com/agustinustheo/polkameter) plugins. PolkaMeter runs load and stress tests on Polkadot SDK chains. A plugin is a separate executable that prepares transactions and checks for one runtime. It exchanges one JSON message per line with PolkaMeter through standard input and output.

Use the default features for the plugin protocol. Add `chain` or `observers` when you need them:

```toml
polkameter-plugin-sdk = "0.3"                                             # protocol only
polkameter-plugin-sdk = { version = "0.3", features = ["chain"] }         # chain access
polkameter-plugin-sdk = { version = "0.3", features = ["observers"] }     # chain access, recorders and checks
```

The `chain` feature reads runtime metadata and signs transactions for supported account types and transaction extensions. The `observers` feature records metrics and returns check results. It turns on `chain` too. Without features, the crate does not depend on subxt.

The [API documentation](https://docs.rs/polkameter-plugin-sdk/0.3) lists every item. The [substrate plugin](https://github.com/agustinustheo/polkameter/tree/main/plugins/substrate) is built on the `chain` feature.

Version 0.3 replaces the 0.2 crates `polkameter-chain`, `polkameter-files`, `polkameter-checks` and `polkameter-monitors`. See [Migrating from 0.2](https://agustinustheodorus.com/polkameter/plugins.html#migrating-from-02). [Writing a plugin](https://agustinustheodorus.com/polkameter/writing-a-plugin.html) walks through a first plugin.
