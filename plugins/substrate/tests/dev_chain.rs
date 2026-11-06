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

/// Both tests fund from //Alice; one at a time, so they never sign with the same nonce.
static ALICE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
	let _alice = ALICE.lock().await;
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
	// The seed's own account holds nothing, so //Alice funds the signers. The transfers step
	// signs with signers of another derivation, so they are funded too.
	for derivation in ["//polkameter//", "//transfers//"] {
		let fund = with(json!({"amount": amount, "funder": "//Alice", "derivation": derivation}));
		let funded = call(&mut plugin, "fund-accounts", fund.clone(), &dir).await;
		assert_eq!(funded["accounts"], 3);
		let again = call(&mut plugin, "fund-accounts", fund, &dir).await;
		assert_eq!(again["funded"], 0, "funding twice tops up nothing: {again}");
		assert_eq!(again["skipped"], 3);
	}
	call(&mut plugin, "preflight", with(json!({"min-balance": amount})), &dir).await;

	// A 65536-block era starts at a multiple of 16; sign at a block that is not one, so the
	// transactions must use an earlier block's hash.
	let client = Client::connect(&target).await.unwrap();
	while client.finalized_now().await.unwrap().block_number() % 16 == 0 {
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
	for (operation, extra) in [
		(
			"prepare-remarks",
			json!({"count": 6, "probes": 2, "payload-bytes": 40, "mortality": 65536, "expected-seconds": 600}),
		),
		(
			"prepare-transfers",
			json!({"count": 6, "probes": 2, "tip": 1, "derivation": "//transfers//"}),
		),
	] {
		let prepared = call(&mut plugin, operation, with(extra), &dir).await;
		assert_eq!(prepared["summary"]["flood"], 6, "{operation}");
		if operation == "prepare-remarks" {
			let at = prepared["summary"]["preparedAt"].as_u64().unwrap();
			let until = prepared["summary"]["validUntil"].as_u64().unwrap();
			assert_eq!(until - 65_536, at - at % 16, "{prepared}");
		}
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

	// The remarks step's signers again: their nonces are taken.
	let error = plugin
		.invoke("prepare-remarks", with(json!({"count": 6, "probes": 2})), &context(&dir))
		.await
		.unwrap_err()
		.to_string();
	assert!(error.contains("an earlier prepare step of this run also signs with"), "{error}");

	// A mortality shorter than the run is refused before anything is signed.
	let error = plugin
		.invoke(
			"prepare-remarks",
			with(json!({"count": 6, "probes": 2, "derivation": "//short//", "mortality": 16, "expected-seconds": 3600})),
			&context(&dir),
		)
		.await
		.unwrap_err()
		.to_string();
	assert!(error.contains("less than expected-seconds 3600"), "{error}");
	std::fs::remove_dir_all(dir).unwrap();
}

/// A funder that holds the top-ups plus 1 cannot also pay the fees and keep its existential
/// deposit. Before, the batch failed on chain and the step reported success; now it fails
/// before it sends anything, and a second attempt sends nothing either.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a dev chain: scripts/dev-chain.sh start"]
async fn a_funder_short_of_fees_fails_before_sending() {
	let _alice = ALICE.lock().await;
	let target = endpoint();
	let dir =
		std::env::temp_dir().join(format!("polkameter-dev-chain-funder-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let base = format!("//Alice//dev-chain-funder//{}", std::process::id());
	let mut plugin = Substrate::default();
	let amount = 1_000_000_000_000u64;
	// Three signers need 3 x amount. Give the funder, a signer of another seed, that plus 1.
	let funder_seed = format!("{base}//funder");
	let funder = format!("{funder_seed}//polkameter//0");
	let give = json!({"target": target, "seed": funder_seed, "funder": "//Alice", "amount": 3 * amount + 1, "flood-signers": 1, "probe-signers": 1});
	call(&mut plugin, "fund-accounts", give, &dir).await;
	let client = Client::connect(&target).await.unwrap();
	let funder_account = polkameter_plugin_sdk::chain::account_id(
		&polkameter_plugin_sdk::chain::keypair(&funder).unwrap(),
	);
	let before = client.account(funder_account).await.unwrap();
	assert_eq!(before.spendable(), u128::from(3 * amount + 1));

	let fund = json!({"target": target, "seed": format!("{base}//signers"), "funder": funder, "amount": amount, "flood-signers": 2, "probe-signers": 1});
	for attempt in 0..2 {
		let error = plugin
			.invoke("fund-accounts", fund.clone(), &context(&dir))
			.await
			.unwrap_err()
			.to_string();
		eprintln!("attempt {attempt}: {error}");
		assert!(error.contains("Nothing was sent"), "attempt {attempt}: {error}");
		assert!(error.contains("the existential deposit"), "{error}");
		let after = client.account(funder_account).await.unwrap();
		assert_eq!(after, before, "attempt {attempt} spent the funder's balance");
	}
	std::fs::remove_dir_all(dir).unwrap();
}
