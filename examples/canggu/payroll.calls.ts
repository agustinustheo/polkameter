// Canggu load: Payroll.batch_pay. Transaction i pays the 3 test accounts after its signer (distinct, never itself).
import type { TypedApi } from "polkadot-api"
import type { canggu } from "@polkadot-api/descriptors"

type Ctx = { api: TypedApi<typeof canggu>; accounts: string[]; account: string; i: number }

export const chain = "canggu"
const AMOUNT = 10n ** 12n // 1000 × the existential deposit, so every credit is valid

export const call = ({ api, accounts, account }: Ctx) => {
	const me = accounts.indexOf(account)
	const payments = [1, 2, 3].map((k) => [accounts[(me + k) % accounts.length], AMOUNT] as [string, bigint])
	return api.tx.Payroll.batch_pay({ payments })
}
