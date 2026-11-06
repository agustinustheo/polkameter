# The substrate plugin

The substrate plugin loads any Polkadot SDK chain that has the standard `System` and `Balances` pallets. It signs `System.remark` or `Balances.transfer_keep_alive` transactions from accounts derived from one seed, funds those accounts, and checks the chain's state after the run. It reads the runtime's metadata, so you need no plugin code for your runtime. [Stress-test a local dev chain](dev-chain.md) runs it end to end.

## Example

The plan below funds 16 flood signers and one probe signer from the seed account, signs 5,000 remarks and sends them at rising rates. [`examples/substrate-remarks.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/main/examples/substrate-remarks.polkameter.xml) is the complete file.

```xml
<plugins><plugin id="substrate" version="0.3.0" protocol="1"/></plugins>
<targets><target id="chain" endpoint="ws://127.0.0.1:9944"/></targets>
<credentials><credential id="seed" profile="substrate-dev"/></credentials>
<setup>
  <step id="fund" use="substrate.fund-accounts" timeout-ms="300000">
    <input name="target" ref="targets.chain"/>
    <input name="seed" ref="credentials.seed"/>
    <input name="flood-signers" value="16"/>
    <input name="amount" value="1000000000000000"/>
  </step>
  <step id="prepare" use="substrate.prepare-remarks" timeout-ms="300000">
    <input name="target" ref="targets.chain"/>
    <input name="seed" ref="credentials.seed"/>
    <input name="flood-signers" value="16"/>
    <input name="count" value="5000"/>
    <input name="probes" ref="run.probeBudget"/>
  </step>
</setup>
<load target="chain" source-ref="steps.prepare.flood" probes-ref="steps.prepare.probes"
      state-check="substrate.check-state" state-ref="steps.prepare.state" block-interval-seconds="6">
  <ramp start="10" step="10" steps="5" seconds="30"/>
</load>
```

`polkameter init --rpc <endpoint>` writes a plan like this one for your chain. See [`init`](#generate-a-plan).

## Install

From v0.3.0, the release archives, the install script and the container image hold the plugin executable, `polkameter-substrate-plugin`, next to `polkameter`. From a checkout, build it, then install it from the checkout's root:

```sh
cargo build -p polkameter-substrate-plugin
polkameter plugin install target/debug/polkameter-substrate-plugin
```

```text
Installed plugin substrate 0.3.0 (5 operations) from ~/polkameter/target/debug/polkameter-substrate-plugin
```

The plugin's version is the PolkaMeter version it ships with, such as `0.3.0`. The plan's `<plugin version>` must match it.

## Signers and secrets

Every operation that signs derives its accounts from `seed`, a secret URI such as `//Alice` or `<mnemonic>//stress`. Pass it from a credential profile, never as a literal value:

```sh
polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
export POLKAMETER_SUBSTRATE_SEED='//Alice'
```

```text
{
  "configured": true
}
```

The plugin derives `flood-signers` accounts for the flood, then `probe-signers` accounts for the probes, by appending an index to `derivation`. With the defaults, the flood signers are `<seed>//polkameter//0` to `<seed>//polkameter//15` and the probe signer is `<seed>//polkameter//16`. The two sets never share an account.

> **Warning:** Use the seed's accounts for one run at a time. Two runs that sign with the same accounts take the same nonces, and one run's transactions replace the other's.

Transaction `i` of the flood goes to flood signer `i mod flood-signers`, with that signer's next nonce. The signers take turns, so each signer's nonces reach the pool in order.

## Operations

Every operation takes `target`, the chain's `ws://` or `wss://` endpoint. Amounts are in the chain's smallest unit, as a whole number or as a string of digits for amounts above 2<sup>64</sup>.

### preflight

Checks the chain without changing it: the runtime has each of `calls`, the plugin can sign for its transaction extensions, and, with `min-balance`, every signer holds at least that much. Read-only, so it can run in `<preflight>`.

| Input | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `target` | string | yes | none | The endpoint |
| `calls` | list of strings | no | `["System.remark", "Balances.transfer_keep_alive"]` | Calls that must exist, as `Pallet.call` |
| `seed` | string | no | none | Secret URI of the signers' seed. Needed for `min-balance` |
| `min-balance` | amount | no | none | The free balance every signer must hold |
| `derivation`, `flood-signers`, `probe-signers` | | no | see [prepare-remarks](#prepare-remarks) | The signer layout |

| Output | Meaning |
| --- | --- |
| `identity` | The chain name, `specName`, `specVersion`, `transactionVersion`, genesis hash, token symbol and decimals |
| `calls` | The calls checked |
| `signers` | `count`, and the first 50 signers with their role, derivation path, address, free balance and nonce |

### fund-accounts

Tops up every flood and probe signer to `amount` from the funder, then waits until the transfers are final. A signer that already holds `amount` is skipped, so running it again sends nothing. With `Utility.batch_all` in the runtime, it sends up to `batch-size` transfers per transaction.

| Input | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `target` | string | yes | none | `ws://` or `wss://` | The endpoint |
| `seed` | string | yes | none | a secret URI | The signers' seed |
| `amount` | amount | yes | none | above 0 | The free balance each signer should hold |
| `funder` | string | no | `seed` | a secret URI | The account that pays. It must not be a signer |
| `batch-size` | integer | no | `100` | 1 to 1000 | Transfers per `Utility.batch_all` |
| `timeout-seconds` | integer | no | `120` | 1 to 3600 | How long to wait for the transfers to be final. Keep it below the step's `timeout-ms` |
| `derivation`, `flood-signers`, `probe-signers` | | no | see [prepare-remarks](#prepare-remarks) | | The signer layout |

| Output | Meaning |
| --- | --- |
| `funded` | Signers topped up |
| `skipped` | Signers that already held `amount` |
| `accounts` | All signers |
| `transactions` | Hashes of the funding transactions |

The funder must hold the total top-up plus fees. Otherwise the step fails before it sends anything, naming the funder and the sum.

### prepare-remarks

Signs the flood and the probes as `System.remark` transactions. Each payload is unique to the run, the lane and the index. Use its outputs as the load's `source-ref`, `probes-ref` and `state-ref`.

| Input | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `target` | string | yes | none | `ws://` or `wss://` | The endpoint |
| `seed` | string | yes | none | a secret URI | The signers' seed |
| `count` | integer | yes | none | 1 to 1,000,000 | Flood transactions |
| `probes` | integer | yes | none | 1 to 100,000 | Probe transactions. Pass `run.probeBudget` |
| `flood-signers` | integer | no | `16` | 1 to 10,000, at most `count` | Accounts that sign the flood |
| `probe-signers` | integer | no | `1` | 1 to 1000, at most `probes` | Accounts that sign the probes |
| `derivation` | string | no | `//polkameter//` | starts and ends with `/` | The path each signer's index is appended to |
| `payload-bytes` | integer | no | `32` | 1 to 16,384 | Remark size |
| `with-event` | boolean | no | `false` | | Use `System.remark_with_event` |
| `mortality` | integer | no | `0` | `0`, or a power of two from 4 to 65,536 | Blocks each transaction stays valid, from the block it was signed at. `0` is immortal |
| `tip` | amount | no | `0` | | Tip per transaction |

The prepared transactions must fit in 200 MiB of artifacts. The step fails before it signs anything if `count` and `payload-bytes` exceed that.

| Output | Meaning |
| --- | --- |
| `flood` | Artifact of the flood's `PreparedTx` list |
| `probes` | Artifact of the probes' `PreparedTx` list |
| `state` | Artifact mapping each transaction hash to its signer and nonce, for `check-state` |
| `identity` | As in [preflight](#preflight) |
| `summary` | The call, the counts, the signers, the block it signed at, the mortality and the transfer amount |

Before it returns, the step asks the chain to validate the first and last flood transactions and the first probe. A signer without funds fails here with `the signer cannot pay the fees; fund it with substrate.fund-accounts`.

### prepare-transfers

As [prepare-remarks](#prepare-remarks), but each transaction is `Balances.transfer_keep_alive`. Instead of `payload-bytes` and `with-event`, it takes:

| Input | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `amount` | amount | no | the existential deposit | Amount per transfer |
| `dest` | string | no | the next signer in the same set | One destination for every transfer, as an SS58 address or `0x` and 64 hex digits |

Without `dest`, each flood signer pays the next flood signer, and each probe signer the next probe signer. The transfers create no accounts. Fund the signers with enough for their transfers and fees.

### check-state

The load's state check. For each sampled transaction, it reads its signer's account at the finalized block `at` and counts the transaction as missing unless the account's nonce is past the transaction's nonce. Read-only.

| Input | Type | Meaning |
| --- | --- | --- |
| `target` | string | Set by PolkaMeter |
| `state` | artifact | The `state` output of the prepare step, from `state-ref` |
| `hashes` | list of strings | Set by PolkaMeter: the sampled transactions |
| `at` | string | Set by PolkaMeter: the finalized block |

It returns `checked`, `missing` and `detail`. `detail` names up to five missing transactions.

## Generate a plan

`polkameter init` connects to a chain, checks that it has the calls the plugin needs, measures its block time and writes a plan for the substrate plugin:

```sh
polkameter init --rpc ws://127.0.0.1:9944 --profile substrate-dev --output target/init.polkameter.xml
```

```text
Wrote target/init.polkameter.xml for Custom (parachain-template-runtime 1, 6 s blocks). Next: polkameter validate target/init.polkameter.xml
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--rpc` | required | The chain's `ws://` or `wss://` endpoint |
| `--kind` | `remarks` | `remarks` or `transfers` |
| `--profile` | `substrate` | The credential profile the plan names for the seed |
| `--output` | standard output | The plan file. An existing file needs `--force` |

The plan ramps from 10 to 50 transactions per second and funds each signer with 1000 tokens. Change the rates and the amount to suit your chain. If the chain cannot be reached, `init` exits with code `3`:

```text
PolkaMeter: cannot connect to ws://127.0.0.1:1: connect: RPC error: client error: RPC error: client error: Error when opening the TCP socket: Connection refused (os error 61)
```

## Limits

- The plugin signs with sr25519 keys and the chain's default transaction extensions. A runtime with an extension that carries data the plugin does not know fails in `preflight` with `the substrate plugin cannot sign transactions for this runtime`. Write a plugin for that runtime: [Writing a plugin](writing-a-plugin.md).
- The state check proves that each sampled transaction's nonce was used. It does not read remark contents or balances.
