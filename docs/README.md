# PolkaMeter

PolkaMeter sends transactions to a Polkadot SDK chain at rising rates. It measures how the chain responds and reports where it stops keeping up.

A plan is an XML file that describes the test. Plugins are programs that do the runtime-specific work, such as building transactions. The [substrate plugin](substrate-plugin.md#limits) covers runtimes with the standard `System` and `Balances` pallets. For other runtimes, you [write a plugin](writing-a-plugin.md).

New here? Start with [Getting started](getting-started.md), then [run it in CI](ci.md).
