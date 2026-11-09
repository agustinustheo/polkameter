# PolkaMeter

PolkaMeter sends transactions to a Polkadot SDK chain at rates you choose. It measures how the chain responds and reports where it stops keeping up.

Plugins are programs that supply the steps that depend on your runtime, such as building a transaction. The [substrate plugin](substrate-plugin.md#limits) covers compatible runtimes with the standard `System` and `Balances` pallets. For other runtimes, you [write a plugin](writing-a-plugin.md).

New here? Start with [Getting started](getting-started.md), then [run it in CI](ci.md).
