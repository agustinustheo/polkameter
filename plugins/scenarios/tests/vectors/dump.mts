// Golden vectors from the TS stress tests, for the Rust port. Reads the TS package only.
// Run from packages/polkameter-tests: `pnpm exec tsx ../../stress/crates/scenarios/tests/vectors/dump.mts`
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { initCrypto, blake2_256 } from "../../../../../packages/coinage/src/index.ts";
import * as claim from "../../../../../packages/polkameter-tests/src/scenarios/stmt/claim.ts";
import { makePeople } from "../../../../../packages/polkameter-tests/src/scenarios/shared/people.ts";
import * as verifiable from "../../../../../packages/polkameter-tests/node_modules/verifiablejs/pkg-nodejs/verifiablejs.js";

await initCrypto();
const hex = (b: Uint8Array) => "0x" + Buffer.from(b).toString("hex");
const u8 = (n: number, f: (i: number) => number) => Uint8Array.from({ length: n }, (_, i) => f(i));
const runSeed = u8(32, (i) => i + 1);
const people = makePeople(runSeed, 3);
const chain = { specVersion: 3003000, txVersion: 1, genesisHash: u8(32, () => 0x11), networkSuffix: "testnet" };
const period = 20722, seq = 5, target = u8(32, () => 0xaa);
const call = claim.setStatementStoreAccountCall(period, seq, target);
const context = claim.stmtStoreContext(chain.networkSuffix, period, seq);
const message = claim.claimMessage(chain, call);
const fakeProof = u8(785, (i) => i % 256);
const tx = claim.buildClaimTx(call, fakeProof, 4, 3);
// A real proof over a ring of the 3 people, for cross-version checks.
const keys = people.map((p) => Buffer.from(p.key.slice(2), "hex"));
const members = Buffer.concat([Buffer.from([3 << 2]), ...keys]);
const shot = verifiable.one_shot(9, people[0].entropy, members, context, message);
const root = verifiable.members_root(9, members);
const out = {
  runSeed: hex(runSeed), chain: { ...chain, genesisHash: hex(chain.genesisHash) }, period, seq, target: hex(target),
  people: people.map((p) => ({ index: p.index, entropy: hex(p.entropy), key: p.key })),
  call: hex(call), context: hex(context), message: hex(message), fakeProofTx: hex(tx), txHash: hex(blake2_256(tx)),
  ring: { members: hex(members), jsRoot: hex(root) }, jsProof: hex(shot.proof), jsAlias: hex(shot.alias),
};
writeFileSync(join(import.meta.dirname, "claim.json"), JSON.stringify(out, null, 2));
console.log("wrote claim.json; proof", shot.proof.length, "root", root.length);
