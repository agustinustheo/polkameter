// Darwinia (AccountId20) load: Assets.transfer_keep_alive of asset 1026. PolkaMeter derives and funds the
// test accounts from the seed, signs, sends at the rate and reconciles; this file only says what to send.
import type { TypedApi, PolkadotClient } from "polkadot-api"
import type { darwinia } from "@polkadot-api/descriptors"

export const chain = "darwinia"

type Ctx = { api: TypedApi<typeof darwinia>; client: PolkadotClient; accounts: string[]; seedAccount: string; i: number }

const ASSET = 1026n
const AMOUNT = 10n ** 12n // ≤ 1e15 and well under the asset's supply (else pallet-assets reports BalanceLow)
const BALANCE = 10n ** 24n // per account: enough for far more than 50 transfers

// Before the clock: give each test account an asset-1026 balance. Asset creation and minting are
// permissioned on Darwinia, so on this Chopsticks fork the balance is written with dev_setStorage.
export async function setup({ api, client, accounts }: Ctx) {
	const entries = accounts.map((a) => [[Number(ASSET), a], { balance: BALANCE.toString(), status: "Liquid", reason: "Sufficient", extra: null }])
	await client._request("dev_setStorage", [{ Assets: { Account: entries } }])
	await client._request("dev_newBlock", [{ count: 1 }])
	for (const a of accounts) {
		const acc = await api.query.Assets.Account.getValue(ASSET, a, { at: "finalized" })
		if (!acc || acc.balance < BALANCE) throw new Error(`asset ${ASSET} balance of ${a} not set (got ${acc?.balance})`)
	}
}

// Transaction i: account i % accounts sends AMOUNT of asset 1026 to the seed account.
export const call = ({ api, seedAccount }: Ctx) =>
	api.tx.Assets.transfer_keep_alive({ id: ASSET, target: seedAccount, amount: AMOUNT })
