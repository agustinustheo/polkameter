# Calls mode

Calls mode loads a chain with your own calls, without a plan or a plugin. You say what to send. PolkaMeter does the rest: it funds the test accounts, signs every call, sends at a constant rate and reconciles each transaction against the finalized chain.

Calls mode is a prototype on the `feat/calls-mode` branch.

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
- `chain`, optional. The PAPI descriptor to use, when `@polkadot-api/descriptors` holds more than one chain.

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

## One call without a file

For a call whose arguments need no code, skip the file:

```sh
polkameter calls --chain ws://127.0.0.1:9944 --seed //Alice --rate 5 --duration 60s \
  --call TemplatePallet.do_something --args '{"bn": "$i"}'
```

`--args` is a JSON array of the arguments in order, or an object by name. `"$i"` is the transaction's index and `"$account"` its signer. Numbers and digit strings are integers, `0x` strings are bytes, and an object with one capitalized key is a variant, such as `{"Id": "$account"}`.

## What PolkaMeter does

- Derives `--accounts` test accounts from `--seed` (`//polkameter//0`, `//polkameter//1`, and so on), plus probe accounts, and tops each up from the seed.
- Signs each call against a finalized block with a mortality that covers the run, at consecutive nonces per account. It never skips or re-signs a nonce. After a dropped connection, it sends the same signed bytes again.
- Sends at `--rate` for `--duration`. If it cannot keep the rate, the verdict is CANNOT TELL, not PASS.
- Reconciles every hash against the finalized chain: finalized, failed in its block, refused, lost or unresolved. A transaction is lost only with proof: its signer's finalized nonce passed it, or its mortality ended before the finalized head, and no finalized block has it.
- Measures the time from submit to the first finalized head at or above each transaction's block.

It signs for `AccountId32` runtimes with sr25519 keys and for `AccountId20` runtimes with Ethereum keys (for example `--seed //Alith`, or `0x` and a private key). It fills each transaction extension the metadata lists. Extensions it doesn't know get the default value of their type, such as `None`. If a default is wrong, the node refuses the first call and the error names those extensions.

## Exit codes

| Code | Verdict | Meaning |
| --- | --- | --- |
| 0 | PASS | Every transaction resolved, none lost, p95 submit to finalized at most `--max-p95` |
| 1 | FAIL | The run's verdict is FAIL, a transaction is proven lost, or the p95 is above `--max-p95` |
| 4 | CANNOT TELL | The load was not delivered at the rate, a transaction is unresolved, or nothing could be timed |
| 3 | | Setup failed (connection, funding, the call file), before the load |
| 2 | | Invalid flags |

The run directory under `--output` (default `polkameter-runs`) holds the usual files, plus `calls-verdict.json` and the generated `plan.polkameter.xml`. `transactions.jsonl` has each transaction's `finalizedMs`.

## Flags

| Flag | Default | What it does |
| --- | --- | --- |
| `--chain` | | WebSocket endpoint |
| `--seed` | `$POLKAMETER_SEED` | Funds the test accounts |
| `--accounts` | 8 | Test accounts that sign the calls |
| `--rate` | | Transactions per second |
| `--duration` | | How long to send, such as `60s` or `2m` |
| `--calls` | | The call file |
| `--call`, `--args` | | One call and its JSON arguments, instead of a file |
| `--max-p95` | `60s` | Limit on p95 submit to finalized |
| `--fund` | a share of the seed's balance | Spendable amount each test account is topped up to |
| `--mortality` | covers the run | Blocks each transaction stays valid, a power of two up to 4096; 0 for immortal |
| `--recovery` | 60 | Seconds to watch the chain after the load |
| `--output` | `polkameter-runs` | Where the run directory goes |
| `--plugin` | next to `polkameter` | The substrate plugin executable |

## Limits

- One constant rate. Use a plan for rate steps.
- Calls are built before the clock starts, for every transaction, and signed at fixed nonces. A call that must read the chain state at send time doesn't fit.
- A mortality above 4096 blocks isn't supported.
- Signers are sr25519 on `AccountId32` or Ethereum ECDSA on `AccountId20`. A runtime with another signature, or an extension whose value must be computed (not a default), needs a plugin.
