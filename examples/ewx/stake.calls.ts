// Energy Web X: LiquidStaking.stake at the chain's minimum (MinStakingAmount, 150 EWT).
// PolkaMeter derives, funds and signs for the test accounts, paces the load and reconciles every hash.
import type { TypedApi } from "polkadot-api"
import type { ewx } from "@polkadot-api/descriptors"

type Ctx = { api: TypedApi<typeof ewx>; data: bigint }

export const chain = "ewx"

// Read the minimum from the runtime once, before the clock starts.
export const setup = ({ api }: Ctx) => api.constants.LiquidStaking.MinStakingAmount()

export const call = ({ api, data }: Ctx) => api.tx.LiquidStaking.stake({ amount: data })
