// A calls-mode load test of the template pallet on scripts/dev-chain.sh. Say only WHAT to send;
// PolkaMeter funds and signs for the test accounts, sends at the rate, and reconciles every hash.
//   npx papi add dev -w ws://127.0.0.1:9944
//   polkameter calls --chain ws://127.0.0.1:9944 --seed //Alice --rate 2 --duration 60s --calls ./template.calls.ts
import type { TypedApi } from "polkadot-api"
import type { dev } from "@polkadot-api/descriptors"

type Ctx = { api: TypedApi<typeof dev>; accounts: string[]; i: number; submit: (tx: unknown, from?: number) => Promise<unknown> }

// Optional, before the clock: each test account stores a first value (signed and awaited by PolkaMeter).
export async function setup({ api, accounts, submit }: Ctx) {
	await Promise.all(accounts.map((_, n) => submit(api.tx.TemplatePallet.do_something({ bn: 1000 + n }), n)))
}

// With several chains in @polkadot-api/descriptors, name one: export const chain = "dev"

// Transaction i stores i.
export const call = ({ api, i }: Ctx) => api.tx.TemplatePallet.do_something({ bn: i })
