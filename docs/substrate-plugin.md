# The substrate plugin

The substrate plugin signs transactions from accounts derived from one seed: `System.remark`, a call that only stores a note, or `Balances.transfer_keep_alive`. It funds those accounts and checks the chain's state after the run. It reads the runtime's metadata, so you need no plugin code. Its version is the PolkaMeter version it ships with, and the plan's `<plugin version>` must match it.

The runtime needs the standard `System` and `Balances` pallets. [Limits](#limits) lists what else it needs. [Getting started](getting-started.md) runs the plugin end to end.

## Example

[`examples/substrate-remarks.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/main/examples/substrate-remarks.polkameter.xml) is a complete plan, and [Generate a plan](#generate-a-plan) writes one for your chain.

## Signers and secrets

Every operation that signs derives its accounts from `seed`, a secret URI such as `//Alice` or `<mnemonic>//stress`. Pass it from a [credential profile](cli.md#plugin), never as a literal value.

The plugin derives `flood-signers` accounts for the flood, then `probe-signers` accounts for the probes. Each account's path is `derivation` plus its index, such as `//polkameter//0`. By default, each probe has its own signer, up to 1000, so a dropped probe never holds back the next one. The two sets never share an account.

Give the `fund-accounts` step and the prepare step the same `seed`, `derivation`, `flood-signers`, `probe-signers` and `probes`. Otherwise, the plugin can prepare transactions for unfunded accounts.

> **Warning:** Use the seed's accounts for one run at a time. Two runs that sign with the same accounts take the same nonces, and one run's transactions replace the other's.

## Operations

Every operation takes `target`, the chain's `ws://` or `wss://` endpoint. Amounts are in the chain's smallest unit, such as planck on Polkadot. Write them as a whole number, or as a string of digits above 2^64.

### preflight

Checks the chain without changing it, so it can run in `<preflight>`. It checks that the runtime has each of `calls` and that the plugin can sign for it. With `min-balance`, it also checks that every signer can spend at least that much.

| Input | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `target` | string | yes | none | The endpoint |
| `calls` | list of strings | no | `["System.remark", "Balances.transfer_keep_alive"]` | Calls that must exist, as `Pallet.call` |
| `seed` | string | no | none | Secret URI of the signers' seed. Needed for `min-balance` |
| `min-balance` | amount | no | none | The spendable balance every signer must hold |
| `derivation`, `flood-signers`, `probe-signers`, `probes` | | no | see [prepare-remarks](#prepare-remarks) | The signer layout |

It returns `identity` (the chain name, runtime version, genesis hash and token), `calls`, and `signers` with the first 50 signers and their balances.

### fund-accounts

Tops up every flood and probe signer to a spendable balance of `amount` from the funder. With `Utility.batch_all` in the runtime, it sends up to `batch-size` transfers per transaction.

| Input | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `target` | string | yes | none | `ws://` or `wss://` | The endpoint |
| `seed` | string | yes | none | a secret URI | The signers' seed |
| `amount` | amount | yes | none | at least the existential deposit | The spendable balance each signer should hold |
| `funder` | string | no | `seed` | a secret URI | The account that pays. It must not be a signer |
| `batch-size` | integer | no | `100` | 1 to 1000 | Transfers per `Utility.batch_all` |
| `timeout-seconds` | integer | no | `120` | 1 to 3600 | How long to wait for the transfers to be final. Keep it below the step's `timeout-ms` |
| `probes` | integer | no | none | 1 to 100,000 | The prepare step's `probes`. Pass `run.probeBudget`, so that each probe's signer is funded |
| `derivation`, `flood-signers`, `probe-signers` | | no | see [prepare-remarks](#prepare-remarks) | | The signer layout |

It returns `funded`, `skipped` (signers that could already spend `amount`), `accounts` and the funding `transactions`.

Running it again only tops up what is missing. Insufficient funder balance fails the step with `Nothing was sent`, and a failed transfer fails with `funding failed`.

### prepare-remarks

Signs the flood and the probes as `System.remark` transactions. Each payload is unique to its run, transaction list (flood or probes) and index. Use its outputs as the load's `source-ref`, `probes-ref` and `state-ref`.

| Input | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `target` | string | yes | none | `ws://` or `wss://` | The endpoint |
| `seed` | string | yes | none | a secret URI | The signers' seed |
| `count` | integer | yes | none | 1 to 1,000,000 | Flood transactions |
| `probes` | integer | yes | none | 1 to 100,000 | Probe transactions. Pass `run.probeBudget` |
| `flood-signers` | integer | no | `16` | 1 to 10,000, at most `count` | Accounts that sign the flood |
| `probe-signers` | integer | no | `probes`, at most 1000 | 1 to 1000, at most `probes` | Accounts that sign the probes |
| `derivation` | string | no | `//polkameter//` | starts and ends with `/` | The path each signer's index is appended to |
| `payload-bytes` | integer | no | `32` | 1 to 16,384 | Remark size |
| `with-event` | boolean | no | `false` | | Use `System.remark_with_event` |
| `mortality` | integer | no | `0` | `0`, or a power of two from 4 to 65,536 | How many blocks a transaction stays valid. `0` never expires |
| `expected-seconds` | integer | no | none | 1 to 604,800 | How long the plan runs after this step. With `mortality`, the step fails if the transactions would expire sooner |
| `tip` | amount | no | `0` | | Tip per transaction |

The flood, the probes and the state must fit in 200 MiB of artifacts together.

A mortal transaction must stay valid for the rest of the run: the baseline, the load, the recovery and the wait for finality. Set `expected-seconds` to that time, and the step fails if the transactions would expire sooner. `polkameter init` sets both inputs.

It returns `flood` and `probes`, the [`PreparedTx`](plugins.md#preparedtx-and-the-load-contract) lists, and `state`, which maps each transaction to its signer and nonce. It also returns `identity` and a `summary` with the signing block and `validUntil`, the expiry block.

Before it returns, the step asks the chain to validate the first and last flood transactions and the first probe of every probe signer. A signer without funds fails here with `the signer cannot pay the fees; fund it with substrate.fund-accounts`.

### prepare-transfers

As [prepare-remarks](#prepare-remarks), but each transaction is `Balances.transfer_keep_alive`. Instead of `payload-bytes` and `with-event`, it takes:

| Input | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `amount` | amount | no | the existential deposit | Amount per transfer |
| `dest` | string | no | the next signer in the same set | One destination for every transfer, as an SS58 address or `0x` and 64 hex digits |

Without `dest`, each signer pays the next signer in its set, so the transfers create no accounts. With `dest`, every transfer pays that account. If it doesn't exist yet, the first transfer must be at least the existential deposit. Fund the signers with enough for their transfers and fees.

### check-state

The load's [state check](plans.md#load), named by `state-check`. PolkaMeter calls it with the prepare step's `state`, the sampled transaction hashes and a finalized block. For each sampled transaction, it reads the signer's nonce at that block. It counts the transaction as missing unless the nonce moved past it. Read-only.

It returns `checked`, `missing` and `detail`. `detail` names up to five missing transactions.

## Generate a plan

[`polkameter init`](cli.md#init) writes a plan for the substrate plugin.

- The ramp goes from 10 to 50 transactions per second.
- Each signer gets 1000 whole tokens.
- `mortality` covers twice the blocks the run spans.
- `expected-seconds` is the run's length after the prepare step. If you lengthen the ramp or the recovery, raise it.

[Getting started](getting-started.md) walks through it.

## Limits

- The plugin signs with sr25519 keys. The runtime's address type must be `MultiAddress` or `AccountId32`, and its signature type `MultiSignature`, as in the parachain template. It fills in the chain's default transaction extensions. Other runtimes, such as one with Ethereum-style `AccountId20` accounts, fail in `preflight` with `the substrate plugin cannot sign transactions for this runtime`. [Write a plugin](writing-a-plugin.md) for them.
- Signed transactions commit to the runtime version. Don't upgrade the runtime during a load test, or the chain refuses the remaining transactions as having a bad signature. It refuses expired transactions the same way.
- The state check proves that each sampled transaction's nonce was used. It does not read remark contents or balances.
