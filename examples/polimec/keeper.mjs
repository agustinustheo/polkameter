// Chopsticks-only sidecar, started by run.sh beside PolkaMeter. usage: node keeper.mjs <ws> <log-file>
// A Polimec staking-round boundary block (every Round.length blocks) spends the whole block on on_initialize,
// so a transaction applied in it is Invalid::ExhaustsResources. A real node keeps it in the pool for the next
// block; Chopsticks drops it, so the signer's later nonces never land. As soon as the block before a boundary is
// final, this builds the boundary block itself (dev_newBlock), before a load transaction can be put into it.
// It never signs or sends a transaction. On a node without dev_newBlock it exits at once.
import { appendFileSync } from "node:fs"
import { createClient } from "polkadot-api"
import { getWsProvider } from "polkadot-api/ws"
import { polimec } from "@polkadot-api/descriptors"

const [ws, file] = process.argv.slice(2)
const log = (m) => appendFileSync(file, `${new Date().toISOString()} ${m}\n`)
const client = createClient(getWsProvider(ws))
const api = client.getTypedApi(polimec)
const { methods } = await client._request("rpc_methods", [])
if (!methods.includes("dev_newBlock")) { log("not Chopsticks: keeper off"); process.exit(0) }
log("keeper on")
let built = 0
client.finalizedBlock$.subscribe(async ({ number, hash }) => {
	try {
		const r = await api.query.ParachainStaking.Round.getValue({ at: hash })
		if (number + 1 !== r.first + r.length) return
		await client._request("dev_newBlock", [{}])
		log(`built round-boundary block #${number + 1} (${++built} so far)`)
	} catch (e) {
		log(`at #${number}: ${e?.message ?? e}`)
	}
})
