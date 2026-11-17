// Polimec load: LinearRelease.vested_transfer, one vesting schedule per transaction, each to its own target
// account (MaxVestingSchedules caps schedules per target). PolkaMeter funds, signs, sends and reconciles.
import { AccountId, Enum, type TypedApi } from "polkadot-api"
import type { polimec } from "@polkadot-api/descriptors"

type Api = TypedApi<typeof polimec>
type Ctx = {
	api: Api; i: number; count: number
	data: { start: number; locked: bigint; salt: number }
	submit: (tx: unknown, from?: number) => Promise<unknown>
}
const target = (salt: number, i: number) => {
	const b = new Uint8Array(32).fill(0x7e)
	new DataView(b.buffer).setUint32(0, salt)
	new DataView(b.buffer).setUint32(4, i)
	return AccountId().dec(b)
}

export async function setup({ api, count, submit }: Ctx) {
	const min = await api.constants.LinearRelease.MinVestedTransfer()
	const locked = min > 10n ** 12n ? min : 10n ** 12n
	// a run-unique salt keeps targets fresh if the chain is reused
	const salt = Date.now() % 2 ** 32
	// vested_transfer places a hold on the target, which needs an existing account: the seed funds every target
	const ed = await api.constants.Balances.ExistentialDeposit()
	const calls = Array.from({ length: count }, (_, i) =>
		api.tx.Balances.transfer_keep_alive({ dest: Enum("Id", target(salt, i)), value: ed * 100n }).decodedCall)
	await submit(api.tx.Utility.batch_all({ calls }))
	return { start: (await api.query.System.Number.getValue()) + 10, locked, salt }
}

export const call = ({ api, i, data }: Ctx) =>
	api.tx.LinearRelease.vested_transfer({
		target: target(data.salt, i),
		schedule: { locked: data.locked, per_block: 10n ** 9n, starting_block: data.start },
		reason: Enum("ParachainStaking", Enum("StakingDelegator")),
	})
