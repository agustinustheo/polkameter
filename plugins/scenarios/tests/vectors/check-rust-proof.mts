// verifiablejs (what the TS tool proves with) validates a proof made by verifiable 0.3.0.
// First `STRESS_WRITE_PROOF=1 cargo test -p polkameter-scenarios --test ts_vectors` writes rust-proof.hex;
// then from packages/polkameter-tests: `pnpm exec tsx ../../stress/crates/scenarios/tests/vectors/check-rust-proof.mts`
import { readFileSync } from "node:fs";
import { join } from "node:path";
import * as verifiable from "../../../../../packages/polkameter-tests/node_modules/verifiablejs/pkg-nodejs/verifiablejs.js";
const v = JSON.parse(readFileSync(join(import.meta.dirname, "claim.json"), "utf8"));
const b = (h: string) => Buffer.from(h.replace(/^0x/, ""), "hex");
const proof = b(readFileSync(join(import.meta.dirname, "rust-proof.hex"), "utf8"));
const alias = verifiable.validate(9, proof, b(v.ring.members), b(v.context), b(v.message));
console.log("verifiablejs validates the Rust proof; alias matches:", Buffer.from(alias).equals(b(v.jsAlias)));
