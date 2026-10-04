//! Spike A: the Rust claim and people match the TS tool byte for byte, and ring proofs made by
//! verifiablejs (git f65b39d) and by verifiable 0.3.0 validate against each other's rings.

use parity_scale_codec::{Decode, Encode};
use polkameter_chain::ChainInfo;
use polkameter_scenarios::shared::people::make_people;
use polkameter_scenarios::stmt::claim::{self, Unproved};
use serde_json::Value;
use verifiable::GenerateVerifiable;
use verifiable::ring::bandersnatch::BandersnatchVrfVerifiable as Vrf;
use verifiable::ring::{
	RingDomainSize, StaticChunk, ark_vrf::ring::SrsLookup,
	ark_vrf::suites::bandersnatch::BandersnatchSha512Ell2, ring_verifier_builder_params,
};

fn vectors() -> Value {
	serde_json::from_str(include_str!("vectors/claim.json")).unwrap()
}

fn bytes(v: &Value) -> Vec<u8> {
	hex::decode(v.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

fn arr(v: &Value) -> [u8; 32] {
	bytes(v).try_into().unwrap()
}

fn chain(v: &Value) -> ChainInfo {
	let c = &v["chain"];
	ChainInfo {
		spec_version: c["specVersion"].as_u64().unwrap() as u32,
		tx_version: c["txVersion"].as_u64().unwrap() as u32,
		genesis: arr(&c["genesisHash"]),
	}
}

#[test]
fn people_match_ts() {
	let v = vectors();
	let people = make_people(&arr(&v["runSeed"]), 3);
	for (p, t) in people.iter().zip(v["people"].as_array().unwrap()) {
		assert_eq!(p.entropy.to_vec(), bytes(&t["entropy"]), "entropy of person {}", p.index);
		assert_eq!(
			p.key.to_vec(),
			bytes(&t["key"]),
			"member key of person {} (verifiable 0.3.0 vs verifiablejs)",
			p.index
		);
	}
}

#[test]
fn claim_bytes_match_ts() {
	let v = vectors();
	let chain = chain(&v);
	let (period, seq, target) = (
		v["period"].as_u64().unwrap() as u32,
		v["seq"].as_u64().unwrap() as u32,
		arr(&v["target"]),
	);
	assert_eq!(claim::call(period, seq, &target), bytes(&v["call"]));
	assert_eq!(claim::context("testnet", period, seq).to_vec(), bytes(&v["context"]));
	let unproved = Unproved::new(&chain, period, seq, &target);
	assert_eq!(unproved.message().to_vec(), bytes(&v["message"]));
	let fake: Vec<u8> = (0..785).map(|i| (i % 256) as u8).collect();
	let tx = unproved.with_proof(&fake, 4, 3);
	assert_eq!(tx, bytes(&v["fakeProofTx"]));
	assert_eq!(polkameter_chain::tx_hash(&tx).to_vec(), bytes(&v["txHash"]));
}

fn rust_root(members: &[[u8; 32]]) -> <Vrf as GenerateVerifiable>::Members {
	let params = ring_verifier_builder_params::<BandersnatchSha512Ell2>(RingDomainSize::Domain11);
	let mut inter = Vrf::start_members(RingDomainSize::Domain11);
	Vrf::push_members(&mut inter, members.iter().copied(), |r| {
		(&params).lookup(r).map(|p| p.into_iter().map(StaticChunk).collect()).ok_or(())
	})
	.unwrap();
	Vrf::finish_members(inter)
}

#[test]
fn proofs_cross_validate() {
	let v = vectors();
	let people = make_people(&arr(&v["runSeed"]), 3);
	let members: Vec<[u8; 32]> = people.iter().map(|p| p.key).collect();
	let (context, message) = (bytes(&v["context"]), bytes(&v["message"]));
	let root = rust_root(&members);
	eprintln!(
		"0.3.0 root: {} bytes; verifiablejs root: {} bytes",
		root.encode().len(),
		bytes(&v["ring"]["jsRoot"]).len()
	);

	// verifiablejs proof, checked by the runtime's crate.
	let js_proof = bytes(&v["jsProof"]);
	let proof =
		<Vrf as GenerateVerifiable>::Proof::decode(&mut &(js_proof.as_slice(),).encode()[..])
			.unwrap();
	let alias = Vrf::validate(RingDomainSize::Domain11, &proof, &root, &context, &message)
		.expect("verifiablejs proof validates with verifiable 0.3.0");
	assert_eq!(alias.to_vec(), bytes(&v["jsAlias"]));

	// verifiable 0.3.0 proof, checked by the same crate (the JS side is checked in check-rust-proof.mts).
	let prover = polkameter_proofs::Prover::new(9, members.clone()).unwrap();
	let ours = prover.open(people[0].entropy).unwrap().prove(&context, &message).unwrap();
	assert_eq!(ours.proof.len(), 785);
	assert_eq!(ours.alias.to_vec(), bytes(&v["jsAlias"]), "same alias in the same context");
	// STRESS_WRITE_PROOF=1 saves the proof for vectors/check-rust-proof.mts, the JS side of this check.
	if std::env::var_os("STRESS_WRITE_PROOF").is_some() {
		std::fs::write(
			concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/rust-proof.hex"),
			hex::encode(&ours.proof),
		)
		.unwrap();
	}
}
