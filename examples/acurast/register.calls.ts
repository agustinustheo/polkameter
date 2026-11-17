// Acurast.register load: each transaction registers one job whose schedule starts in the future.
// PolkaMeter (calls mode) funds the test accounts, signs, sends at the rate and reconciles.
import { Enum } from "polkadot-api"

export const chain = "acurast"

// Before the clock: read the chain's time once; every job starts an hour after it.
export async function setup({ api }: any) {
	return { now: BigInt(await api.query.Timestamp.Now.getValue()) }
}

const script = new TextEncoder().encode("ipfs://QmUXhYmUwjgWtGpU8hXkCRzhZsUqpThFEzYBRDTDw7HBpX")

export const call = ({ api, data, i }: any) => {
	const start = data.now + 3_600_000n + BigInt(i) * 1_000n // ms
	return api.tx.Acurast.register({
		registration: {
			script,
			allowed_sources: undefined,
			allow_only_verified_sources: false,
			schedule: { duration: 60_000n, start_time: start, end_time: start + 3_600_000n, interval: 600_000n, max_start_delay: 0n },
			memory: 0,
			network_requests: 0,
			storage: 0,
			required_modules: [],
			// PAPI flattens RegistrationExtra { requirements }: `extra` is the requirements object itself.
			extra: { assignment_strategy: Enum("Single", undefined), slots: 1, reward: 1_000_000_000n, min_reputation: undefined, processor_version: undefined, runtime: Enum("NodeJS") },
		},
	})
}
