// Myriad load: Tipping.send_tip to one registered server, signed by PolkaMeter's test accounts.
// Setup (before the clock): the seed registers a server once (Server.register), unless it already owns one.
import type { TypedApi } from "polkadot-api"
import type { myriad } from "@polkadot-api/descriptors"

type Ctx = { api: TypedApi<typeof myriad>; seedAccount: string; i: number; data: string; submit: (tx: unknown, from?: number) => Promise<unknown> }

export const chain = "myriad"
const text = (s: string) => new TextEncoder().encode(s)

export async function setup({ api, seedAccount, submit }: Ctx) {
	const owned = await api.query.Server.ServerByOwner.getEntries(seedAccount)
	if (owned.length === 0) await submit(api.tx.Server.register({ api_url: text(`https://stress.myriad.test/${Date.now()}`), stake_amount: undefined }))
	return seedAccount // the server_id of a tip is the server owner's account
}

export const call = ({ api, i, data }: Ctx) =>
	api.tx.Tipping.send_tip({
		info: { server_id: data, reference_type: text("people"), reference_id: text(`stress-${i}`), ft_identifier: text("native") },
		amount: 10n ** 18n, // 1 MYRIA; at least the existential deposit (0.1), or the tip fails with Token.BelowMinimum
	})
