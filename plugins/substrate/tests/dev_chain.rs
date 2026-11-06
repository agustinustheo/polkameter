//! The plugin against a live dev chain: fund, prepare, submit, and check the state. Ignored by
//! default because it needs a chain. Start one and run it with:
//!
//! ```sh
//! scripts/dev-chain.sh start
//! POLKAMETER_DEV_CHAIN_WS=ws://127.0.0.1:9944 cargo test -p polkameter-substrate-plugin --test dev_chain -- --ignored
//! scripts/dev-chain.sh stop
//! ```

use std::time::Duration;

use polkameter_plugin_sdk::{Artifact, Context, Plugin, PreparedTx, chain::Client, strip_0x};
use polkameter_substrate_plugin::Substrate;
use serde_json::{Value, json};

fn endpoint() -> String {
	std::env::var("POLKAMETER_DEV_CHAIN_WS").unwrap_or_else(|_| "ws://127.0.0.1:9944".into())
}

fn context(dir: &std::path::Path) -> Context {
	Context::host(format!("run-dev-chain-{}", std::process::id()), dir)
}

async fn call(
	plugin: &mut Substrate,
	operation: &str,
	inputs: Value,
	dir: &std::path::Path,
) -> Value {
	let manifest = polkameter_plugin_sdk::Plugin::manifest(plugin);
	let contract = &manifest.operations[operation];
	contract.validate_inputs(&inputs).unwrap();
	let result = plugin
		.invoke(operation, inputs, &context(dir))
		.await
		.unwrap_or_else(|e| panic!("{operation}: {e:#}"));
	contract.validate_outputs(&result).unwrap();
	result
}

/// Waits until every transaction in `hashes` is in a finalized block; returns that block.
async fn finalized_with(client: &Client, hashes: &[String]) -> [u8; 32] {
	let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
	let mut seen = std::collections::HashSet::new();
	let mut number = client.best_number().await.unwrap().saturating_sub(2);
	loop {
		let at = client.finalized().await.unwrap();
		let head = at.block_number() as u32;
		while number <= head {
			for bytes in client.body(client.block_hash(number).await.unwrap()).await.unwrap() {
				seen.insert(hex::encode(polkameter_plugin_sdk::chain::tx_hash(&bytes)));
			}
			number += 1;
		}
		if hashes.iter().all(|h| seen.contains(strip_0x(h))) {
			return at.block_hash().0;
		}
		assert!(tokio::time::Instant::now() < deadline, "transactions were not finalized in 120 s");
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a dev chain: scripts/dev-chain.sh start"]
async fn fund_prepare_submit_and_check() {
	let target = endpoint();
	let dir = std::env::temp_dir().join(format!("polkameter-dev-chain-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	// A seed of its own, so the test never races the example plans for nonces.
	let seed = format!("//Alice//dev-chain-test//{}", std::process::id());
	let signers = json!({"flood-signers": 2, "probe-signers": 1});
	let with = |extra: Value| {
		let mut inputs = json!({"target": target, "seed": seed});
		for source in [&signers, &extra] {
			for (k, v) in source.as_object().unwrap() {
				inputs[k] = v.clone();
			}
		}
		inputs
	};
	let mut plugin = Substrate::default();

	let preflight = call(&mut plugin, "preflight", with(json!({})), &dir).await;
	assert_eq!(preflight["signers"]["count"], 3);

	let amount = 1_000_000_000_000_000u64;
	// The seed's own account holds nothing, so //Alice funds the signers.
	let fund = with(json!({"amount": amount, "funder": "//Alice"}));
	let funded = call(&mut plugin, "fund-accounts", fund.clone(), &dir).await;
	assert_eq!(funded["accounts"], 3);
	let again = call(&mut plugin, "fund-accounts", fund, &dir).await;
	assert_eq!(again["funded"], 0, "funding twice tops up nothing: {again}");
	assert_eq!(again["skipped"], 3);
	call(&mut plugin, "preflight", with(json!({"min-balance": amount})), &dir).await;

	for (operation, extra) in [
		("prepare-remarks", json!({"count": 6, "probes": 2, "payload-bytes": 40, "mortality": 64})),
		("prepare-transfers", json!({"count": 6, "probes": 2, "tip": 1})),
	] {
		let prepared = call(&mut plugin, operation, with(extra), &dir).await;
		assert_eq!(prepared["summary"]["flood"], 6, "{operation}");
		let flood: Vec<PreparedTx> = serde_json::from_value::<Artifact>(prepared["flood"].clone())
			.unwrap()
			.read(&dir)
			.unwrap();
		let probes: Vec<PreparedTx> =
			serde_json::from_value::<Artifact>(prepared["probes"].clone())
				.unwrap()
				.read(&dir)
				.unwrap();
		assert!(flood.iter().all(|tx| !probes.iter().any(|p| p.hash == tx.hash)));

		let client = Client::connect(&target).await.unwrap();
		let mut hashes = Vec::new();
		for tx in &flood {
			hashes.push(client.submit(&tx.decode().unwrap()).await.unwrap());
		}
		let at = finalized_with(&client, &hashes).await;
		let check = |hashes: Vec<String>| json!({"target": target, "state": prepared["state"], "hashes": hashes, "at": hex::encode(at)});
		let sent: Vec<String> = flood.iter().map(|tx| tx.hash.clone()).collect();
		let result = call(&mut plugin, "check-state", check(sent), &dir).await;
		assert_eq!(
			(result["checked"].clone(), result["missing"].clone()),
			(json!(6), json!(0)),
			"{result}"
		);
		// The probes were never sent, so their nonces are still ahead of the chain.
		let unsent: Vec<String> = probes.iter().map(|tx| tx.hash.clone()).collect();
		let result = call(&mut plugin, "check-state", check(unsent), &dir).await;
		assert_eq!(result["missing"], 2, "{result}");
	}
	std::fs::remove_dir_all(dir).unwrap();
}
