// Mythos Nfts.mint load for `polkameter calls`. Setup: each test account creates its own collection
// (Issuer mint type, so the owner may mint). Transaction i: test account i % accounts mints the next
// item of its own collection to itself. PolkaMeter funds, signs, sends and reconciles.
import { Enum } from "polkadot-api"

export const chain = "mythos"

type Ctx = { api: any; accounts: string[]; account: string; data: Record<string, unknown>; submit: (tx: unknown, from?: number) => Promise<any> }

const config = {
	settings: 0n,
	max_supply: undefined,
	mint_settings: { mint_type: Enum("Issuer"), price: undefined, start_block: undefined, end_block: undefined, default_item_settings: 0n, serial_mint: true },
}

export async function setup({ api, accounts, submit }: Ctx) {
	const collections: Record<string, unknown> = {}
	// One at a time, so each Nfts.Created event (or the owner's collection) is unambiguous.
	for (const [n, admin] of accounts.entries()) {
		const r = await submit(api.tx.Nfts.create({ admin, config }), n)
		let id = r?.events?.find((e: any) => e.type === "Nfts" && e.value?.type === "Created")?.value?.value?.collection
		if (id === undefined) {
			const owned = await api.query.Nfts.CollectionAccount.getEntries(admin, { at: "finalized" })
			id = owned.at(-1)?.keyArgs[1]
		}
		if (id === undefined) throw new Error(`no collection found for ${admin} after Nfts.create`)
		collections[admin.toLowerCase()] = id
	}
	return collections
}

export const call = ({ api, account, data }: Ctx) =>
	api.tx.Nfts.mint({ collection: data[account.toLowerCase()], maybe_item: undefined, mint_to: account, witness_data: undefined })
