# PolkaMeter

PolkaMeter sends transactions to a Polkadot SDK chain and reports whether the chain kept up. It checks every transaction against the finalized chain, so it blames the chain only for the chain's faults.

You write a test in one of two ways:

- [Calls mode](calls-mode.md): you write what to send, as a [polkadot-api](https://papi.how) call. PolkaMeter funds the test accounts, signs, sends at a constant rate and checks every transaction. Use it for your own pallets' calls.
- A [plan](plans.md): an XML file that raises the rate one rate step at a time to find where the chain stops keeping up. Plugins are programs that build the transactions. The [substrate plugin](substrate-plugin.md#limits) covers runtimes with the standard `System` and `Balances` pallets. For other calls, use calls mode or [write a plugin](writing-a-plugin.md).

New here? Start with [Getting started](getting-started.md), then [run it in CI](ci.md).
