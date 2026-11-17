// Aventus (AvN): TokenManager.transfer of the AVT token from each test account to the seed account.
import type { TypedApi } from "polkadot-api"
import type { aventus } from "@polkadot-api/descriptors"

type Ctx = { api: TypedApi<typeof aventus>; seedAccount: string; data: { token: `0x${string}` } }

export const chain = "aventus"

// The AVT token contract, read once before the clock.
export async function setup({ api }: Ctx) {
	const token = await api.query.TokenManager.AVTTokenContract.getValue({ at: "finalized" })
	if (!token || /^0x0+$/.test(token)) throw new Error("TokenManager.AVTTokenContract is not set")
	return { token }
}

// 0.001 AVT (18 decimals) per transaction.
export const call = ({ api, seedAccount, data }: Ctx) =>
	api.tx.TokenManager.transfer({ to: seedAccount, token_id: data.token, amount: 10n ** 15n })
