# Calls mode

Calls mode loads a chain with your own calls, without a plan or a plugin. You say what to send. PolkaMeter does the rest: it funds the test accounts, signs every call, sends at a constant rate and reconciles each transaction against the finalized chain.

## Run a call file

A call file is a JavaScript or TypeScript module. It builds calls with [polkadot-api](https://papi.how) (PAPI) and its generated types. You need Node.js 22.18 or later; Node.js runs `.ts` files without a build step.

In the directory of the call file, install PAPI and generate the types for your chain:

```sh
npm install polkadot-api
npx papi add dev -w ws://127.0.0.1:9944
```

Write `calls.ts`. It exports `call`, which returns transaction `i`:

```ts
export const call = ({ api, i }) => api.tx.TemplatePallet.do_something({ bn: i })
```

Run it:

```sh
polkameter calls --chain ws://127.0.0.1:9944 --seed //Alice \
  --accounts 8 --rate 5 --duration 60s --calls ./calls.ts --max-p95 30s
```

`examples/calls/template.calls.ts` is a complete call file for the dev chain of `scripts/dev-chain.sh`.

## The call file

The call file exports:

- `call(ctx)`, required. It returns transaction `ctx.i`: a PAPI transaction, call data as `0x` hex, or a `Uint8Array`.
- `setup(ctx)`, optional. It runs once, before the clock starts. Whatever it returns is `ctx.data` in `call`.
- `chain`, optional. The chain to use when `@polkadot-api/descriptors` holds more than one: the descriptor, or its key as a string, such as `export const chain = "dev"` (the name given to `papi add`).

`ctx` holds:

| Field | What it is |
| --- | --- |
| `api` | The PAPI typed API of the chain |
| `client` | The PAPI client |
| `i` | The transaction's index, from 0 (`call` only) |
| `account` | The test account that signs transaction `i`: SS58 for 32-byte accounts, `0x` hex for 20-byte ones (`call` only) |
| `accounts` | Every test account |
| `seedAccount` | The account of `--seed` |
| `count` | How many calls PolkaMeter asks for |
| `data` | What `setup` returned (`call` only) |
| `submit(tx, from)` | Sends a setup transaction and resolves once it is final; rejects if it failed. `from` is a test account index; without it, the seed signs |

Transaction `i` is signed by test account `i % accounts`. Calls must not depend on each other's order across accounts.

`call` runs once per transaction before the clock starts, and PolkaMeter reads the calls one by one, so their size has no limit. One run signs at most 100,000 calls (rate × duration + 5%, plus one per account); above that, `polkameter calls` refuses the flags with exit 2.

## What PolkaMeter does

- Checks the runtime and measures the block time from blocks built after it starts, watching for up to 30 seconds. A chain that builds no block in that time, such as a Chopsticks fork that builds blocks only on demand, gets the runtime's slot duration (or 6 seconds), and PolkaMeter prints that it did.
- Derives `--accounts` test accounts from `--seed` (`//polkameter//0`, `//polkameter//1`, and so on), plus at most 4 probe accounts, and tops each up from the seed.
- Signs each call against a finalized block, at consecutive nonces per account. It never skips or re-signs a nonce. After a dropped connection, it sends the same signed bytes again.
- Signs with the longest mortality the chain keeps: 4096 blocks, or less when `System.BlockHashCount` is lower. It doesn't depend on the block time, which on some chains follows the load (a Chopsticks fork builds a block per transaction).
- Opens a dropped connection again before and during setup, for up to 90 seconds, and repeats the read or sends the same signed bytes. Funding and setup transactions are never sent twice. When the connection doesn't come back, it exits 3 and names the cause.
- Prints progress at least every 30 seconds, including the substrate plugin's progress during setup, so that no wait is silent.
- Sends at `--rate` for `--duration`: exactly rate × duration transactions, each at its own time from the start. If fewer than 95% of them are sent, the verdict is CANNOT TELL, not PASS. Transactions it could not send during an outage are not sent later.
- After the load, keeps watching the chain until every transaction is in a block, for at most `--recovery` seconds.
- Reconciles every hash against the finalized chain and gives each one category (see [Output](#output)).
- Measures the time from submit to the first finalized head at or above each transaction's block.

It signs for `AccountId32` runtimes with sr25519 keys and for `AccountId20` runtimes with Ethereum keys (for example `--seed //Alith`, or `0x` and a private key). It fills each transaction extension the metadata lists. Extensions it doesn't know get the default value of their type, such as `None`. If a default is wrong, the node refuses the first call and the error names those extensions.

## Exit codes

| Code | Verdict | Meaning |
| --- | --- | --- |
| 0 | PASS | The load was delivered, every transaction resolved, none lost, and the p95 is proven at most `--max-p95` |
| 1 | FAIL | The chain is proven to miss a threshold on the load it was given |
| 4 | CANNOT TELL | Not a proven FAIL, and the load was not delivered or something is unresolved |
| 3 | | Setup failed (connection, funding, the call file, an unsupported runtime), before the load |
| 2 | | Invalid flags |

### How the verdict is decided

One rule, in this order. The first that holds decides.

1. **FAIL** if the chain is proven to cause a problem on the load it was given:
   - a transaction is `lost`, or
   - the p95 submit to finalized is proven above `--max-p95`: more than 5% of the transactions sent took longer.

   Only two kinds of time count as proof. The first is a time PolkaMeter measured. The second is the age of a transaction that the node's pool still held ready when the run ended, after its first submit; a chain that holds a ready transaction and builds no block for it has stalled.
2. **CANNOT TELL** if any of these holds:
   - the load was not delivered: fewer than 95% of rate × duration transactions were first submitted in the load window;
   - a transaction is `unresolved`;
   - the node `refused` a transaction;
   - more than 5% of the transactions failed in their blocks (dispatch errors), which usually means the call is wrong for the chain;
   - the p95 is not proven within the limit, because transactions PolkaMeter could not time might be slower.
3. **PASS** otherwise.

The verdict is computed from `transactions.jsonl` alone. PolkaMeter's own problems can't give a FAIL: its reconnects and pauses (a transaction it could not time proves nothing), a load it could not deliver, its probes, or a chain that builds no block until a transaction arrives, such as a Chopsticks fork. The engine's own verdict can FAIL on these. `summary.md` starts with the calls verdict and keeps the engine's report below it as diagnostics; `calls-verdict.json` has the engine's result as `engineVerdict`, which is not used.

SIGINT or SIGTERM stops the run, the substrate plugin and the call file's runner, within the shutdown grace of 20 seconds. The run has no verdict and exits 130 or 143.

## Output

The run directory under `--output` (default `polkameter-runs`) holds the usual files, plus `calls-verdict.json` and the generated `plan.polkameter.xml`.

`transactions.jsonl` has one line per transaction sent. These fields audit it against the chain:

| Field | What it is |
| --- | --- |
| `hash` | The transaction's hash (blake2-256 of its bytes), hex |
| `category` | Where it ended, see below |
| `signer` | Its signer, decoded from its bytes: SS58 (generic prefix 42, as `ctx.account`) for 32-byte accounts, `0x` hex for 20-byte ones |
| `nonce` | The nonce it was signed with, decoded from its bytes |
| `mortality` | `{"birth": block, "period": blocks}`: the block it was signed at and how long it is valid. `"immortal"` without mortality; `null` when it could not be decoded |
| `firstSubmitMs` | When PolkaMeter first handed it to a connection, in Unix ms. A resend after a reconnect keeps it. It equals `sentAt` |
| `finalizedMs` | Time from `firstSubmitMs` to the first finalized head at or above its block; `null` when PolkaMeter could not time it (its pauses or reconnects) |
| `finalizedMsAtMost` | Time from `firstSubmitMs` until PolkaMeter saw it final: an upper bound, set even when `finalizedMs` is `null` |
| `inReadyPool`, `poolSnapshotMs` | Whether the node's pool held it ready, and when the pool was listed (Unix ms) |
| `finalizedBlock` | The finalized block it is in |

`category` is one of:

| `category` | Meaning |
| --- | --- |
| `finalized-ok` | In a finalized block, and its dispatch succeeded |
| `failed-in-block` | In a finalized block, and its dispatch failed there |
| `lost` | Proven lost: no finalized block has it, and none can. Its signer's finalized nonce passed it, or its mortality ended after the chain had at least half of it to include it |
| `refused` | The node rejected it at submit; it was never in a block |
| `unresolved` | Anything else: it may still be included, or its end has no proof |

`calls-verdict.json` counts the categories in `categories`, which add up to `sent`. It also has `verdict`, `exitCode`, `reason`, `target` (rate × duration), `sent`, `sentInWindow` (first submitted in the load window, up to 2 s after its end) and `deliveredRate` (`sentInWindow` per second of the duration). The same delivered rate is printed on standard output. For latency it has `p95FinalizedMs` (over the timed transactions) and the proven bounds `p95AtLeastMs` and `p95AtMostMs` (over every transaction sent; `null` means no bound).

A transaction that expired after being sent with less than half its mortality left is `unresolved`, not `lost`: the mortality it was signed with ended it, not the chain.

## Flags

| Flag | Default | What it does |
| --- | --- | --- |
| `--chain` | | WebSocket endpoint |
| `--seed` | `$POLKAMETER_SEED` | Funds the test accounts |
| `--accounts` | 8 | Test accounts that sign the calls |
| `--rate` | | Transactions per second |
| `--duration` | | How long to send, such as `60s` or `2m` |
| `--calls` | | The call file |
| `--max-p95` | `60s` | Limit on p95 submit to finalized, such as `30s`, `29.5s` or `29500ms`. PASS needs the p95 at most this; for p95 < 60 s, pass `59999ms` |
| `--fund` | a share of the seed's balance, at most a million existential deposits (no cap when the existential deposit is 0) | Spendable amount each test account is topped up to |
| `--mortality` | the longest the chain keeps, up to 4096 | Blocks each transaction stays valid, a power of two up to 4096; 0 for immortal |
| `--recovery` | the longer of 300 and the duration | At most this many seconds to watch the chain after the load, until every transaction is in a block |
| `--output` | `polkameter-runs` | Where the run directory goes |
| `--plugin` | next to `polkameter` | The substrate plugin executable |

## Limits

- One constant rate. Use a plan for rate steps.
- Calls are built before the clock starts, for every transaction, and signed at fixed nonces. A call that must read the chain state at send time doesn't fit.
- A mortality above 4096 blocks isn't supported.
- At most 100,000 calls per run, for example 1000 tx/s for 90 s.
- Under overload a node can drop transactions from its pool. With the default long mortality they stay `unresolved`, so the verdict is CANNOT TELL, not FAIL.
- A transaction its chain dropped stays `unresolved` until its signer's finalized nonce passes it or its mortality ends, so the last transaction of a signer can stay `unresolved` with a long mortality.
- After a connection outage during the load, the transactions not sent then are not sent later. The verdict is CANNOT TELL when that leaves the load short.
- Old runtimes are not supported: one without the `Metadata_metadata_versions` runtime API, or with single-value weights (no `proof_size`), exits 3 at once with an error that names the cause.
- Signers are sr25519 on `AccountId32` or Ethereum ECDSA on `AccountId20`. A runtime with another signature, or an extension whose value must be computed (not a default), needs a plugin.
