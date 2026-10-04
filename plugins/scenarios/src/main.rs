//! PR 37's People preparation and assertions as independently callable operations.
use anyhow::{Context as _, Result, ensure};
use polkameter_chain::{Client, Keypair, Value as ScaleValue, fetch, has_prefix};
use polkameter_plugin_sdk::{
	Artifact, Context, Field, Manifest, Operation, PROTOCOL, Plugin, PreparedTx, Schema,
};
use polkameter_proofs::{Job, Prover, ProverPool};
use polkameter_scenarios::{
	setup::signed,
	shared::{
		people::{PEOPLE_COLLECTION, PEOPLE_EXPONENT, make_people},
		rings::wait_for_rings,
	},
	stmt::claim::{self, Unproved},
	tx::expected_extensions,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

#[derive(Default)]
struct People {
	// A plugin lives for one run. Cache only the expensive per-loss diagnostic path.
	check_client: Option<(String, Client)>,
	check_state: Option<(std::path::PathBuf, String, BTreeMap<String, Value>)>,
}
fn operation(
	description: &str,
	inputs: &[(&str, Schema)],
	outputs: &[(&str, Schema)],
	read_only: bool,
) -> Operation {
	Operation {
		description: description.into(),
		inputs: inputs.iter().map(|(k, v)| (k.to_string(), Field::from(v.clone()))).collect(),
		outputs: outputs.iter().map(|(k, v)| (k.to_string(), Field::from(v.clone()))).collect(),
		read_only,
	}
}
fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
	value[name].as_str().with_context(|| format!("missing {name}"))
}
fn number(value: &Value, name: &str) -> Result<u32> {
	value[name]
		.as_u64()
		.and_then(|n| n.try_into().ok())
		.with_context(|| format!("invalid {name}"))
}
fn seed(context: &Context) -> [u8; 32] {
	sp_crypto_hashing::blake2_256(context.run_id.as_bytes())
}
async fn connect(inputs: &Value) -> Result<Client> {
	let client = Client::connect(text(inputs, "target")?).await?;
	client.check_extensions(&expected_extensions().collect::<Vec<_>>()).await??;
	// The proof message includes these bytes; guard the indexes before any setup.
	let encoded = client
		.call_data(
			"Resources",
			"set_statement_store_account",
			vec![ScaleValue::u128(0), ScaleValue::u128(0), ScaleValue::from_bytes([0u8; 32])],
		)
		.await?;
	ensure!(
		encoded == claim::call(0, 0, &[0; 32]),
		"Resources claim encoding is incompatible with this plugin"
	);
	Ok(client)
}
impl Plugin for People {
	fn manifest(&self) -> Manifest {
		Manifest {
			id: "people".into(),
			version: "0.1.0".into(),
			protocol: PROTOCOL,
			requirements: vec![],
			operations: [
				(
					"preflight".into(),
					operation(
						"Check People extension layout and claim encoding",
						&[("target", Schema::String)],
						&[("identity", Schema::Json)],
						true,
					),
				),
				(
					"recognize".into(),
					operation(
						"Prepare signed sudo recognition transactions; the host submits them",
						&[
							("target", Schema::String),
							("credential", Schema::String),
							("members", Schema::Integer),
						],
						&[("transactions", Schema::Json), ("members", Schema::Integer)],
						false,
					),
				),
				(
					"prepare-claims".into(),
					operation(
						"Wait for finalized rings and precompute statement-claim proofs",
						&[
							("target", Schema::String),
							("members", Schema::Integer),
							("slots", Schema::Integer),
							("probe-reserve", Schema::Integer),
						],
						&[
							("flood", Schema::Json),
							("probes", Schema::Json),
							("state", Schema::Json),
							("identity", Schema::Json),
						],
						false,
					),
				),
				(
					"validate-prepared".into(),
					operation(
						"Reject expired or runtime-incompatible prepared claims",
						&[("target", Schema::String), ("identity", Schema::Json)],
						&[("valid", Schema::Boolean)],
						true,
					),
				),
				(
					"check-state".into(),
					operation(
						"Check claim allowance state at the requested finalized block",
						&[
							("target", Schema::String),
							("state", Schema::Json),
							("hashes", Schema::Array { items: Box::new(Schema::String) }),
							("at", Schema::String),
						],
						&[
							("checked", Schema::Integer),
							("missing", Schema::Integer),
							("detail", Schema::String),
						],
						true,
					),
				),
			]
			.into(),
		}
	}
	#[allow(clippy::too_many_lines)]
	async fn invoke(&mut self, operation: &str, inputs: Value, context: &Context) -> Result<Value> {
		let client = if operation == "check-state" {
			let target = text(&inputs, "target")?;
			if self.check_client.as_ref().is_none_or(|(url, _)| url != target) {
				self.check_client = Some((target.to_owned(), connect(&inputs).await?));
			}
			self.check_client.as_ref().unwrap().1.clone()
		} else {
			connect(&inputs).await?
		};
		if operation == "check-state" {
			let result = self.check_state(&client, &inputs, context).await;
			if result.as_ref().is_err_and(|error| error.is::<polkameter_chain::ChainError>()) {
				// A cached connection can drop during a long run. The read is at a fixed block
				// hash, so one retry on a new connection is safe.
				self.check_client = None;
				let client = connect(&inputs).await?;
				self.check_client = Some((text(&inputs, "target")?.to_owned(), client.clone()));
				return self.check_state(&client, &inputs, context).await;
			}
			return result;
		}
		let chain = client.chain_info().await?;
		let identity = json!({"genesis":hex::encode(chain.genesis),"specVersion":chain.spec_version,"transactionVersion":chain.tx_version});
		match operation {
			"preflight" => Ok(json!({"identity":identity})),
			"validate-prepared" => {
				for key in ["genesis", "specVersion", "transactionVersion"] {
					ensure!(
						inputs["identity"][key] == identity[key],
						"prepared claim runtime identity changed"
					);
				}
				let now: u64 = fetch(&client.finalized().await?, "Timestamp", "Now", ())
					.await?
					.context("timestamp missing")?;
				ensure!(
					inputs["identity"]["period"].as_u64() == Some(u64::from(claim::period_of(now))),
					"prepared claim period expired"
				);
				Ok(json!({"valid":true}))
			},
			"recognize" => {
				let members = number(&inputs, "members")?;
				ensure!(members > 0 && members <= 100_000, "members must be between 1 and 100000");
				let signer = Keypair::from_uri(
					&text(&inputs, "credential")?
						.parse()
						.map_err(|_| anyhow::anyhow!("invalid setup credential"))?,
				)
				.map_err(|_| anyhow::anyhow!("invalid setup credential"))?;
				let people = make_people(&seed(context), members);
				let mut nonce = client.nonce(signer.public_key().0).await?;
				let mut txs = Vec::new();
				for batch in people.chunks(500) {
					let keys = ScaleValue::unnamed_composite(
						batch.iter().map(|p| ScaleValue::from_bytes(p.key)),
					);
					let call = client
						.call_data("People", "force_recognize_personhood", vec![keys])
						.await?;
					let tx = signed(&chain, &signer, nonce, client.sudo(&call).await?);
					client.validate(&tx, "recognize people").await?;
					txs.push(PreparedTx::new(&tx, json!({"sudo":true})));
					nonce = nonce.checked_add(1).context("nonce overflow")?;
				}
				Ok(
					json!({"transactions":Artifact::write(&context.artifact_dir,"recognition.json",&txs)?,"members":members}),
				)
			},
			"prepare-claims" => {
				let members = number(&inputs, "members")?;
				let slots = number(&inputs, "slots")?;
				let reserve = number(&inputs, "probe-reserve")? as usize;
				ensure!(
					members > 0 && members <= 100_000 && slots > 0 && slots <= 20,
					"invalid member or slot budget"
				);
				let total = (members as usize)
					.checked_mul(slots as usize)
					.context("claim budget overflow")?;
				ensure!(
					total > reserve && total <= 100_000,
					"claim budget must cover probe reserve and fit 100000 claims"
				);
				let people = make_people(&seed(context), members);
				let keys = people.iter().map(|p| p.key).collect::<Vec<_>>();
				let (rings, ring_of) =
					wait_for_rings(&client, PEOPLE_COLLECTION, &keys, Duration::from_secs(1200))
						.await?;
				let at = client.finalized().await?;
				let now: u64 =
					fetch(&at, "Timestamp", "Now", ()).await?.context("timestamp missing")?;
				let suffix: Vec<u8> = fetch(&at, "NetworkSuffix", "NetworkSuffix", ())
					.await?
					.context("network suffix missing")?;
				let suffix = String::from_utf8(suffix)?;
				let period = claim::period_of(now);
				let provers = rings
					.iter()
					.map(|(i, r)| Ok((*i, Prover::new(PEOPLE_EXPONENT, r.keys.clone())?)))
					.collect::<Result<BTreeMap<_, _>>>()?;
				let opened = tokio::task::block_in_place(|| {
					people
						.iter()
						.enumerate()
						.map(|(i, p)| provers[&ring_of[i]].open(p.entropy))
						.collect::<Result<Vec<_>, _>>()
				})?;
				let mut claims = Vec::new();
				let mut jobs = Vec::new();
				let mut metadata = Vec::new();
				for slot in 0..slots {
					for (member, ring) in ring_of.iter().enumerate() {
						// Keep PR 37's workload target mapping for comparison.
						let target: [u8; 32] = std::array::from_fn(|i| {
							(member as u8) ^ (slot as u8) ^ seed(context)[i]
						});
						let tx = Unproved::new(&chain, period, slot, &target);
						jobs.push(Job {
							member,
							context: claim::context(&suffix, period, slot).to_vec(),
							message: tx.message(),
						});
						claims.push((tx, *ring));
						metadata.push(
							json!({"target":hex::encode(target),"member":member,"slot":slot,"period":period}),
						);
					}
				}
				let pool = ProverPool::new(None);
				let proofs = tokio::task::block_in_place(|| pool.prove_all(&opened, &jobs))?;
				let transactions = claims
					.into_iter()
					.zip(proofs)
					.zip(metadata)
					.map(|(((tx, ring), proof), metadata)| {
						PreparedTx::new(
							&tx.with_proof(&proof.proof, ring, rings[&ring].revision),
							metadata,
						)
					})
					.collect::<Vec<_>>();
				for tx in [&transactions[0], &transactions[total - 1]] {
					client.validate(&tx.decode()?, "prepared claim").await?;
				}
				let after: u64 = fetch(&client.finalized().await?, "Timestamp", "Now", ())
					.await?
					.context("timestamp missing")?;
				ensure!(claim::period_of(after) == period, "claim period changed while proving");
				let latest = client.chain_info().await?;
				ensure!(latest == chain, "runtime changed while proving");
				let (flood, probes) = transactions.split_at(total - reserve);
				let state = transactions
					.iter()
					.map(|t| (t.hash.clone(), t.metadata.clone()))
					.collect::<BTreeMap<_, _>>();
				Ok(
					json!({"flood":Artifact::write(&context.artifact_dir,"claims.json",&flood)?,"probes":Artifact::write(&context.artifact_dir,"probes.json",&probes)?,"state":Artifact::write(&context.artifact_dir,"claim-state.json",&state)?,"identity":{ "genesis":identity["genesis"],"specVersion":chain.spec_version,"transactionVersion":chain.tx_version,"period":period}}),
				)
			},

			_ => anyhow::bail!("unknown People operation"),
		}
	}
}
impl People {
	async fn check_state(
		&mut self,
		client: &Client,
		inputs: &Value,
		context: &Context,
	) -> Result<Value> {
		let state: Artifact = serde_json::from_value(inputs["state"].clone())?;
		if self
			.check_state
			.as_ref()
			.is_none_or(|(path, hash, _)| path != &state.path || hash != &state.blake2)
		{
			self.check_state = Some((
				state.path.clone(),
				state.blake2.clone(),
				state.read(&context.artifact_dir)?,
			));
		}
		let state = &self.check_state.as_ref().unwrap().2;
		let at: [u8; 32] = hex::decode(text(inputs, "at")?.trim_start_matches("0x"))?
			.try_into()
			.map_err(|_| anyhow::anyhow!("invalid block hash"))?;
		let block = client.at(at).await?;
		let hashes = inputs["hashes"].as_array().context("hashes required")?;
		let mut missing = 0;
		for hash in hashes {
			let hash = hash.as_str().context("invalid hash")?.trim_start_matches("0x");
			let metadata = state.get(hash).context("claim hash absent from state mapping")?;
			let target: [u8; 32] = hex::decode(text(metadata, "target")?)?
				.try_into()
				.map_err(|_| anyhow::anyhow!("invalid target"))?;
			if !has_prefix::<([u8; 32], ScaleValue), _>(
				&block,
				"Resources",
				"StmtStoreAllowanceByAccount",
				(target,),
			)
			.await?
			{
				missing += 1;
			}
		}
		Ok(
			json!({"checked":hashes.len(),"missing":missing,"detail":"claim targets have allowance entries at the selected finalized block"}),
		)
	}
}
#[tokio::main]
async fn main() -> Result<()> {
	polkameter_plugin_sdk::serve(People::default()).await
}
