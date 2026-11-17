// Acala Dex load: every transaction swaps exactly 1e15 planck ACA (1000 ACA) for AUSD, accepting any output.
// PolkaMeter derives and funds the test accounts from --seed, signs, sends at the rate and reconciles.
import { Enum } from "polkadot-api"

export const chain = "acala"

export const call = ({ api }: { api: any }) =>
	api.tx.Dex.swap_with_exact_supply({
		path: [Enum("Token", Enum("ACA")), Enum("Token", Enum("AUSD"))],
		supply_amount: 10n ** 15n,
		min_target_amount: 0n,
	})
