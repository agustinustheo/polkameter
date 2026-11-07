//! Use this template when you need a call the substrate plugin does not provide. Use it as
//! `src/main.rs` with this `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! anyhow = "1"
//! polkameter-plugin-sdk = { version = "0.3", features = ["chain"] }
//! serde_json = "1"
//! tokio = { version = "1", features = ["full"] }
//! ```
//!
//! <!-- v0.3.0: remove after release -->
//! Until polkameter-plugin-sdk 0.3 is on crates.io, depend on the repository instead:
//! `polkameter-plugin-sdk = { git = "https://github.com/agustinustheo/polkameter", branch = "main", features = ["chain"] }`.
//!
//! Replace `System.remark` with your call and its fields. Keep secrets in credential profiles:
//! the plan passes the seed as `<input name="seed" ref="credentials.seed"/>`. Fund the flood
//! signer and one signer per probe before a load, for example with the substrate plugin's
//! `fund-accounts` and `derivation` set to `//my-runtime//`.
//!
//! `TxBuilder` signs with sr25519 keys for `MultiAddress` or `AccountId32` accounts, and fills in
//! subxt's default transaction extensions (the FRAME `Check*` extensions, the payment
//! extensions and `CheckMetadataHash`). It cannot sign for a runtime with a transaction
//! extension that carries data subxt does not know, or with `AccountId20` and ECDSA
//! signatures. For such a runtime, build the transactions with subxt itself, version 0.51:
//! define a `subxt::Config` whose `ExtrinsicParams` include your extension (implement
//! `subxt::config::TransactionExtension` for it), and whose
//! `AccountId`, `Address` and `Signature` match the runtime. Then sign each call with
//! `create_signable_offline(&call, params)` on your config's client at a block, and pass the
//! signed bytes to `PreparedTx::new` as below.

use anyhow::{Context as _, Result, bail};
use polkameter_plugin_sdk::{
	Artifact, Context, Manifest, Operation, PROTOCOL, Plugin, PreparedTx, Schema,
	chain::{
		Call, Client, Keypair, Mortality, TxBuilder, Value, account_id, derive_range, keypair,
	},
};
use serde_json::{Value as Json, json};

struct MyRuntime;

impl Plugin for MyRuntime {
	fn manifest(&self) -> Manifest {
		Manifest {
			id: "my-runtime".into(),
			version: "0.1.0".into(),
			protocol: PROTOCOL,
			requirements: vec![],
			operations: [(
				"prepare".into(),
				Operation::new(
					"Sign flood and probe transactions",
					&[
						("target", Schema::String),
						("seed", Schema::String),
						("count", Schema::Integer),
						("probes", Schema::Integer),
					],
					&[("flood", Schema::Json), ("probes", Schema::Json)],
				),
			)]
			.into_iter()
			.collect(),
		}
	}

	async fn invoke(&mut self, operation: &str, inputs: Json, context: &Context) -> Result<Json> {
		match operation {
			"prepare" => prepare(&inputs, context).await,
			other => bail!("unknown operation {other}"),
		}
	}
}

/// The call transaction `index` of `lane` sends. Make each one unique.
fn call(lane: &str, index: u32) -> Call {
	let payload = format!("my-runtime/{lane}/{index}").into_bytes();
	Call::new("System", "remark", vec![Value::from_bytes(payload)])
}

/// `count` transactions signed by `signer`, with consecutive nonces from its next nonce.
async fn lane(
	client: &Client,
	builder: &TxBuilder,
	signer: &Keypair,
	name: &str,
	first: u32,
	count: u32,
) -> Result<Vec<PreparedTx>> {
	let nonce = u64::from(client.nonce(account_id(signer)).await?);
	let calls: Vec<Call> = (first..first + count).map(|i| call(name, i)).collect();
	let signed = builder.sign_batch(signer, nonce, &calls, Mortality::Immortal, 0)?;
	Ok(signed
		.iter()
		.map(|bytes| PreparedTx::new(bytes, json!({"lane": name})))
		.collect())
}

async fn prepare(inputs: &Json, context: &Context) -> Result<Json> {
	let target = inputs["target"].as_str().context("input target is missing")?;
	let count = u32::try_from(inputs["count"].as_u64().context("input count is missing")?)?;
	let probes = u32::try_from(inputs["probes"].as_u64().context("input probes is missing")?)?;
	let seed = keypair(inputs["seed"].as_str().context("input seed is missing")?)?;
	// One signer for the flood, and one per probe. A missing probe must not block the next
	// probe's nonce.
	let signers = derive_range(&seed, "//my-runtime//", 0..1 + probes)?;
	let client = Client::connect(target).await?;
	let builder = client.tx_builder().await?;
	builder.check_signing_types()?;
	let flood = lane(&client, &builder, &signers[0], "flood", 0, count).await?;
	let mut probe_txs = Vec::new();
	for (i, signer) in signers[1..].iter().enumerate() {
		probe_txs.extend(lane(&client, &builder, signer, "probe", i as u32, 1).await?);
	}
	Ok(json!({
		"flood": Artifact::write(&context.artifact_dir, "my-runtime-flood.json", &flood)?,
		"probes": Artifact::write(&context.artifact_dir, "my-runtime-probes.json", &probe_txs)?,
	}))
}

#[tokio::main]
async fn main() -> Result<()> {
	polkameter_plugin_sdk::serve(MyRuntime).await
}
