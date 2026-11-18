# polkameter-plugin-sdk

The SDK for [PolkaMeter](https://github.com/agustinustheo/polkameter) plugins. PolkaMeter runs load and stress tests on Polkadot SDK chains.

```toml
polkameter-plugin-sdk = "0.5"                                             # protocol only
polkameter-plugin-sdk = { version = "0.5", features = ["chain"] }         # chain access
polkameter-plugin-sdk = { version = "0.5", features = ["observers"] }     # chain access, recorders and checks
```

See [the plugin SDK crate](https://agustinustheodorus.com/polkameter/plugins.html#the-plugin-sdk-crate), [migrating from 0.2](https://github.com/agustinustheo/polkameter/blob/main/CHANGELOG.md#migrating-plugins-from-02), [Writing a plugin](https://agustinustheodorus.com/polkameter/writing-a-plugin.html) and the [API documentation](https://docs.rs/polkameter-plugin-sdk/0.5).
