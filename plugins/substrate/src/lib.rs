//! A PolkaMeter plugin for any Polkadot SDK chain with the standard FRAME pallets.
//!
//! It signs `System.remark` or `Balances.transfer_keep_alive` transactions from accounts derived
//! from one seed, funds those accounts, and checks after a run that the sampled transactions
//! changed the chain. It reads the runtime's metadata, so it needs no code for a new runtime. The
//! operations are listed in [`manifest`].

pub mod inputs;

use std::{
	collections::{BTreeMap, HashMap},
	path::PathBuf,
	time::Duration,
};

use anyhow::{Context as _, Result, bail, ensure};
use inputs::{Fund, Kind, Prepare};
use polkameter_plugin_sdk::{
	Artifact, Context, Field, Manifest, Operation, PROTOCOL, Plugin, PreparedTx, Schema,
	StateCheckInput,
	chain::{
		Call, ChainError, Client, Keypair, Mortality, TxBuilder, TxParams, Value as ScaleValue,
		account_at, account_id, hex0x, ss58,
	},
	strip_0x,
};
use serde_json::{Value, json};

/// The plugin's ID in plans.
pub const ID: &str = "substrate";

/// Accounts listed in full in a `preflight` reply; the rest are counted.
const LISTED_ACCOUNTS: usize = 50;

fn field(schema: Schema, optional: bool) -> Field {
	Field { schema, optional }
}

fn operation(
	description: &str,
	required: &[(&str, Schema)],
	optional: &[(&str, Schema)],
	outputs: &[(&str, Schema)],
) -> Operation {
	let mut op = Operation::new(description, required, outputs);
	for (name, schema) in optional {
		op.inputs.insert((*name).into(), field(schema.clone(), true));
	}
	op
}

/// Inputs every operation that derives the load signers accepts.
fn signer_inputs() -> Vec<(&'static str, Schema)> {
	vec![
		("derivation", Schema::String),
		("flood-signers", Schema::Integer),
		("probe-signers", Schema::Integer),
	]
}

fn prepare_operation(description: &str, extra: &[(&'static str, Schema)]) -> Operation {
	let mut optional = signer_inputs();
	optional.extend([("mortality", Schema::Integer), ("tip", Schema::Json)]);
	optional.extend(extra.iter().cloned());
	operation(
		description,
		&[
			("target", Schema::String),
			("seed", Schema::String),
			("count", Schema::Integer),
			("probes", Schema::Integer),
		],
		&optional,
		&[
			("flood", Schema::Json),
			("probes", Schema::Json),
			("state", Schema::Json),
			("identity", Schema::Json),
			("summary", Schema::Json),
		],
	)
}

/// The plugin's manifest: its ID, version and operations.
pub fn manifest() -> Manifest {
	let mut preflight_optional = signer_inputs();
	preflight_optional.extend([
		("seed", Schema::String),
		("calls", Schema::Array { items: Box::new(Schema::String) }),
		("min-balance", Schema::Json),
	]);
	let mut fund_optional = signer_inputs();
	fund_optional.extend([
		("funder", Schema::String),
		("batch-size", Schema::Integer),
		("timeout-seconds", Schema::Integer),
	]);
	Manifest {
		id: ID.into(),
		version: env!("CARGO_PKG_VERSION").into(),
		protocol: PROTOCOL,
		requirements: vec![],
		operations: BTreeMap::from([
			(
				"preflight".into(),
				operation(
					"Check the chain's calls and transaction format, and the load signers' balances",
					&[("target", Schema::String)],
					&preflight_optional,
					&[
						("identity", Schema::Json),
						("calls", Schema::Json),
						("signers", Schema::Json),
					],
				)
				.read_only(),
			),
			(
				"fund-accounts".into(),
				operation(
					"Top up every load signer to amount from the funder; skips signers that hold enough",
					&[
						("target", Schema::String),
						("seed", Schema::String),
						("amount", Schema::Json),
					],
					&fund_optional,
					&[
						("funded", Schema::Integer),
						("skipped", Schema::Integer),
						("accounts", Schema::Integer),
						("transactions", Schema::Array { items: Box::new(Schema::String) }),
					],
				),
			),
			(
				"prepare-remarks".into(),
				prepare_operation(
					"Sign System.remark flood and probe transactions with unique payloads",
					&[("payload-bytes", Schema::Integer), ("with-event", Schema::Boolean)],
				),
			),
			(
				"prepare-transfers".into(),
				prepare_operation(
					"Sign Balances.transfer_keep_alive flood and probe transactions",
					&[("amount", Schema::Json), ("dest", Schema::String)],
				),
			),
			(
				"check-state".into(),
				Operation::new(
					"Check that each sampled transaction's signer nonce is past it at the finalized block",
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
				)
				.read_only(),
			),
		]),
	}
}

/// One prepared transaction's entry in the state artifact: who signed it, with which nonce.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Signed {
	/// The signer, `0x` and 64 hex digits.
	pub signer: String,
	/// The transaction's nonce.
	pub nonce: u64,
}

/// The plugin. One process serves one run.
#[derive(Default)]
pub struct Substrate {
	/// Artifacts written so far, so names stay unique when a plan prepares twice.
	prepared: u32,
	check_client: Option<(String, Client)>,
	check_state: Option<(PathBuf, String, HashMap<String, Signed>)>,
}

impl Plugin for Substrate {
	fn manifest(&self) -> Manifest {
		manifest()
	}

	async fn invoke(&mut self, operation: &str, inputs: Value, context: &Context) -> Result<Value> {
		match operation {
			"preflight" => preflight(&inputs).await,
			"fund-accounts" => fund_accounts(&inputs).await,
			"prepare-remarks" => {
				let spec = Prepare::remarks(&inputs)?;
				self.prepared += 1;
				prepare(&inputs, &spec, context, self.prepared).await
			},
			"prepare-transfers" => {
				let spec = Prepare::transfers(&inputs)?;
				self.prepared += 1;
				prepare(&inputs, &spec, context, self.prepared).await
			},
			"check-state" => self.check_state(&inputs, context).await,
			other => bail!("unknown operation {other}"),
		}
	}
}

async fn connect(target: &str) -> Result<Client> {
	Client::connect(target)
		.await
		.map_err(|e| anyhow::anyhow!("cannot connect to {target}: {e}"))
}

/// The chain's name, runtime identity and token, for the run's records.
async fn identity(client: &Client) -> Result<Value> {
	let info = client.chain_info().await?;
	let chain: String = client.request("system_chain", subxt_rpcs::rpc_params![]).await?;
	let version: Value =
		client.request("state_getRuntimeVersion", subxt_rpcs::rpc_params![]).await?;
	let properties: Value = client
		.request("system_properties", subxt_rpcs::rpc_params![])
		.await
		.unwrap_or(Value::Null);
	Ok(json!({
		"chain": chain,
		"specName": version["specName"],
		"specVersion": info.spec_version,
		"transactionVersion": info.tx_version,
		"genesis": hex0x(info.genesis),
		"tokenSymbol": properties["tokenSymbol"],
		"tokenDecimals": properties["tokenDecimals"],
	}))
}

/// Fails unless the runtime has every call in `calls`.
fn require_calls(builder: &TxBuilder, calls: &[(&str, &str)]) -> Result<()> {
	let missing: Vec<String> = calls
		.iter()
		.filter(|(pallet, call)| !builder.has_call(pallet, call))
		.map(|(pallet, call)| format!("{pallet}.{call}"))
		.collect();
	ensure!(
		missing.is_empty(),
		"the runtime has no {}; the substrate plugin needs the standard System and Balances pallets",
		missing.join(", ")
	);
	Ok(())
}

/// Fails unless the plugin can sign for this runtime: its transaction extensions are ones subxt
/// can fill in.
fn require_signable(builder: &TxBuilder) -> Result<()> {
	let probe = polkameter_plugin_sdk::chain::keypair("//polkameter-preflight")?;
	builder
		.sign(
			&Call::new("System", "remark", vec![ScaleValue::from_bytes([0u8])]),
			&probe,
			&TxParams::nonce(0),
		)
		.map_err(|e| {
			anyhow::anyhow!("the substrate plugin cannot sign transactions for this runtime: {e}")
		})?;
	Ok(())
}

fn describe(error: &ChainError) -> String {
	// `raw` starts with the SCALE `Err(Invalid(..))` of `TransactionValidityError`.
	match error {
		ChainError::Invalid { raw, .. } if raw == "010001" => {
			format!("{error}: the signer cannot pay the fees; fund it with substrate.fund-accounts")
		},
		ChainError::Invalid { raw, .. } if raw == "010003" => {
			format!("{error}: the nonce is already used; another process signs with these accounts")
		},
		other => other.to_string(),
	}
}

async fn preflight(inputs: &Value) -> Result<Value> {
	let target = inputs::target(inputs)?;
	let calls = inputs::calls(inputs)?;
	let signers = inputs::Signers::parse(inputs)?;
	let min_balance = match &inputs["min-balance"] {
		Value::Null => None,
		_ => Some(inputs::amount(inputs, "min-balance", None)?),
	};
	let seed = match &inputs["seed"] {
		Value::Null => None,
		_ => Some(inputs::secret(inputs, "seed")?),
	};
	ensure!(seed.is_some() || min_balance.is_none(), "input min-balance needs input seed");
	let client = connect(&target).await?;
	let builder = client.tx_builder().await?;
	let wanted: Vec<(&str, &str)> = calls.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
	require_calls(&builder, &wanted)?;
	require_signable(&builder)?;
	let mut listed = Vec::new();
	let mut short = Vec::new();
	if let Some(seed) = seed {
		let (flood, probe) = signers.derive(&seed)?;
		let roles = flood.iter().map(|k| ("flood", k)).chain(probe.iter().map(|k| ("probe", k)));
		for (index, (role, key)) in roles.enumerate() {
			let account = account_id(key);
			let info = client.account(account).await?;
			if min_balance.is_some_and(|min| info.free < min) {
				short.push(format!(
					"{} ({}, {} free)",
					ss58(account),
					signers.path(index as u32),
					info.free
				));
			}
			if listed.len() < LISTED_ACCOUNTS {
				listed.push(json!({
					"role": role,
					"path": signers.path(index as u32),
					"account": ss58(account),
					"free": info.free.to_string(),
					"nonce": info.nonce,
				}));
			}
		}
	}
	if let Some(min) = min_balance {
		ensure!(
			short.is_empty(),
			"{} of {} signers hold less than min-balance {min}; run substrate.fund-accounts first. First: {}",
			short.len(),
			signers.flood + signers.probe,
			short.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
		);
	}
	Ok(json!({
		"identity": identity(&client).await?,
		"calls": calls.iter().map(|(p, c)| format!("{p}.{c}")).collect::<Vec<_>>(),
		"signers": {"count": signers.flood + signers.probe, "listed": listed},
	}))
}

/// Whether the runtime's address type is `MultiAddress` (most runtimes) rather than a bare
/// account ID.
fn multi_address(builder: &TxBuilder) -> bool {
	builder.call_data(&transfer_call(true, [0; 32], 1)).is_ok()
}

/// `Balances.transfer_keep_alive(dest, amount)`.
fn transfer_call(multi_address: bool, dest: [u8; 32], amount: u128) -> Call {
	let dest = if multi_address {
		ScaleValue::unnamed_variant("Id", [ScaleValue::from_bytes(dest)])
	} else {
		ScaleValue::from_bytes(dest)
	};
	Call::new("Balances", "transfer_keep_alive", vec![dest, ScaleValue::u128(amount)])
}

async fn fund_accounts(inputs: &Value) -> Result<Value> {
	let fund = Fund::parse(inputs)?;
	let target = inputs::target(inputs)?;
	let seed = inputs::secret(inputs, "seed")?;
	let funder = match &inputs["funder"] {
		Value::Null => seed.clone(),
		_ => inputs::secret(inputs, "funder")?,
	};
	let (flood, probe) = fund.signers.derive(&seed)?;
	let accounts: Vec<[u8; 32]> = flood.iter().chain(&probe).map(account_id).collect();
	let funder_account = account_id(&funder);
	ensure!(
		!accounts.contains(&funder_account),
		"the funder is one of the load signers; use another funder or derivation"
	);
	let client = connect(&target).await?;
	let builder = client.tx_builder().await?;
	require_calls(&builder, &[("Balances", "transfer_keep_alive")])?;
	let mut deficits = Vec::new();
	for account in &accounts {
		let free = client.account(*account).await?.free;
		if free < fund.amount {
			deficits.push((*account, fund.amount - free));
		}
	}
	let skipped = accounts.len() - deficits.len();
	if deficits.is_empty() {
		eprintln!("all {} signers already hold {} or more", accounts.len(), fund.amount);
		return Ok(
			json!({"funded": 0, "skipped": skipped, "accounts": accounts.len(), "transactions": []}),
		);
	}
	let total: u128 = deficits.iter().map(|(_, d)| d).sum();
	let funder_free = client.account(funder_account).await?.free;
	ensure!(
		funder_free > total,
		"the funder {} holds {funder_free}, but topping up {} signers needs {total} plus fees",
		ss58(funder_account),
		deficits.len()
	);
	let multi = multi_address(&builder);
	let transfers: Vec<Call> = deficits
		.iter()
		.map(|(dest, amount)| transfer_call(multi, *dest, *amount))
		.collect();
	let calls: Vec<Call> = if transfers.len() > 1 && builder.has_call("Utility", "batch_all") {
		transfers
			.chunks(fund.batch as usize)
			.map(|chunk| {
				let inner = chunk.iter().map(|c| {
					ScaleValue::unnamed_variant(
						c.pallet.clone(),
						[ScaleValue::unnamed_variant(c.name.clone(), c.fields.clone())],
					)
				});
				Call::new("Utility", "batch_all", vec![ScaleValue::unnamed_composite(inner)])
			})
			.collect()
	} else {
		transfers
	};
	let nonce = u64::from(client.nonce(funder_account).await?);
	let txs = builder.sign_batch(&funder, nonce, &calls, Mortality::Immortal, 0)?;
	client
		.validate(&txs[0], "funding transaction")
		.await
		.map_err(|e| anyhow::anyhow!(describe(&e)))?;
	let mut hashes = Vec::new();
	for tx in &txs {
		hashes.push(client.submit(tx).await.context("submit funding transaction")?);
	}
	eprintln!("submitted {} funding transactions for {} signers", txs.len(), deficits.len());
	// Done when the funder's nonce at the finalized head is past the last funding transaction.
	// Balances alone cannot tell: a signer may still pay for a transaction of an earlier run that
	// lands after its top-up was computed.
	let done = nonce + txs.len() as u64;
	let deadline = tokio::time::Instant::now() + Duration::from_secs(fund.timeout_seconds);
	loop {
		let at = client.finalized_now().await?;
		let funder_nonce = account_at(&at, funder_account).await?.nonce;
		if funder_nonce >= done {
			break;
		}
		ensure!(
			tokio::time::Instant::now() < deadline,
			"funding did not finalize within {} s: {} of {} funding transactions are final; check the funder's transactions {}",
			fund.timeout_seconds,
			funder_nonce.saturating_sub(nonce),
			txs.len(),
			hashes.join(", ")
		);
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
	Ok(json!({
		"funded": deficits.len(),
		"skipped": skipped,
		"accounts": accounts.len(),
		"transactions": hashes,
	}))
}

/// One transaction to sign: who signs it, with which nonce, and the call.
struct Job {
	signer: usize,
	nonce: u64,
	call: Call,
	metadata: Value,
}

/// Signs `jobs` on every core. Signing needs no RPC.
fn sign_all(
	builder: &TxBuilder,
	keys: &[Keypair],
	jobs: &[Job],
	tip: u128,
	mortality: Mortality,
) -> Result<Vec<Vec<u8>>> {
	let threads = std::thread::available_parallelism().map_or(1, usize::from).clamp(1, 16);
	let chunk = jobs.len().div_ceil(threads).max(1);
	let results: Vec<Result<Vec<Vec<u8>>>> = std::thread::scope(|scope| {
		let handles: Vec<_> = jobs
			.chunks(chunk)
			.map(|part| {
				scope.spawn(move || {
					part.iter()
						.map(|job| {
							builder
								.sign(
									&job.call,
									&keys[job.signer],
									&TxParams { nonce: job.nonce, mortality, tip },
								)
								.map_err(anyhow::Error::from)
						})
						.collect::<Result<Vec<_>>>()
				})
			})
			.collect();
		handles
			.into_iter()
			.map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a signing thread panicked"))))
			.collect()
	});
	let mut out = Vec::with_capacity(jobs.len());
	for part in results {
		out.extend(part?);
	}
	Ok(out)
}

/// A unique remark payload of `size` bytes for transaction `index` of `lane`.
pub fn payload(run_id: &str, lane: &str, index: u32, size: u32) -> Vec<u8> {
	let mut out = Vec::with_capacity(size as usize);
	let mut counter = 0u32;
	while out.len() < size as usize {
		let mut seed = format!("polkameter/{run_id}/{lane}/{index}/").into_bytes();
		seed.extend(counter.to_le_bytes());
		out.extend(sp_crypto_hashing::blake2_256(&seed));
		counter += 1;
	}
	out.truncate(size as usize);
	out
}

/// What every transaction of a prepare step shares.
struct Maker<'a> {
	spec: &'a Prepare,
	run_id: &'a str,
	/// The transfer amount; unused for remarks.
	amount: u128,
	/// Whether the runtime's address type is `MultiAddress`.
	multi_address: bool,
}

impl Maker<'_> {
	/// The jobs of one lane: transaction `i` goes to signer `i % keys`, so every signer's nonces
	/// are sent in order and the signers take turns.
	fn jobs(&self, lane: &str, keys: &[Keypair], nonces: &[u64], count: u32) -> Vec<Job> {
		let n = keys.len();
		(0..count)
			.map(|i| {
				let signer = i as usize % n;
				let nonce = nonces[signer] + u64::from(i) / n as u64;
				let call = match &self.spec.kind {
					Kind::Remarks { payload_bytes, with_event } => Call::new(
						"System",
						if *with_event { "remark_with_event" } else { "remark" },
						vec![ScaleValue::from_bytes(payload(self.run_id, lane, i, *payload_bytes))],
					),
					Kind::Transfers { dest, .. } => {
						let dest = dest.unwrap_or_else(|| account_id(&keys[(signer + 1) % n]));
						transfer_call(self.multi_address, dest, self.amount)
					},
				};
				let metadata = json!({"lane": lane, "index": i, "signer": signer, "nonce": nonce});
				Job { signer, nonce, call, metadata }
			})
			.collect()
	}
}

async fn prepare(
	inputs: &Value,
	spec: &Prepare,
	context: &Context,
	sequence: u32,
) -> Result<Value> {
	let target = inputs::target(inputs)?;
	let seed = inputs::secret(inputs, "seed")?;
	let (flood_keys, probe_keys) = spec.signers.derive(&seed)?;
	let client = connect(&target).await?;
	let builder = client.tx_builder().await?;
	let (pallet, call) = match spec.kind {
		Kind::Remarks { with_event: true, .. } => ("System", "remark_with_event"),
		Kind::Remarks { .. } => ("System", "remark"),
		Kind::Transfers { .. } => ("Balances", "transfer_keep_alive"),
	};
	require_calls(&builder, &[(pallet, call)])?;
	require_signable(&builder)?;
	let amount = match spec.kind {
		Kind::Transfers { amount: Some(amount), .. } => amount,
		Kind::Transfers { amount: None, .. } => client.existential_deposit().await?,
		Kind::Remarks { .. } => 0,
	};
	let mortality = spec.mortality.map_or(Mortality::Immortal, |period| builder.mortal(period));
	let nonces = |keys: &[Keypair]| {
		let client = client.clone();
		let accounts: Vec<[u8; 32]> = keys.iter().map(account_id).collect();
		async move {
			let mut out = Vec::with_capacity(accounts.len());
			for account in accounts {
				out.push(u64::from(client.nonce(account).await?));
			}
			Ok::<_, ChainError>(out)
		}
	};
	let flood_nonces = nonces(&flood_keys).await?;
	let probe_nonces = nonces(&probe_keys).await?;
	let maker =
		Maker { spec, run_id: &context.run_id, amount, multi_address: multi_address(&builder) };
	let flood_jobs = maker.jobs("flood", &flood_keys, &flood_nonces, spec.count);
	let probe_jobs = maker.jobs("probe", &probe_keys, &probe_nonces, spec.probes);
	let started = std::time::Instant::now();
	let (flood_bytes, probe_bytes) = tokio::task::block_in_place(|| {
		Ok::<_, anyhow::Error>((
			sign_all(&builder, &flood_keys, &flood_jobs, spec.tip, mortality)?,
			sign_all(&builder, &probe_keys, &probe_jobs, spec.tip, mortality)?,
		))
	})?;
	eprintln!(
		"signed {} flood and {} probe transactions in {:.1} s",
		flood_bytes.len(),
		probe_bytes.len(),
		started.elapsed().as_secs_f64()
	);
	for (what, bytes) in [
		("first flood transaction", &flood_bytes[0]),
		("last flood transaction", &flood_bytes[flood_bytes.len() - 1]),
		("first probe transaction", &probe_bytes[0]),
	] {
		client.validate(bytes, what).await.map_err(|e| anyhow::anyhow!(describe(&e)))?;
	}
	let mut state = BTreeMap::new();
	let mut lane = |keys: &[Keypair], jobs: Vec<Job>, bytes: Vec<Vec<u8>>| -> Vec<PreparedTx> {
		jobs.into_iter()
			.zip(bytes)
			.map(|(job, bytes)| {
				let tx = PreparedTx::new(&bytes, job.metadata);
				state.insert(
					tx.hash.clone(),
					Signed { signer: hex0x(account_id(&keys[job.signer])), nonce: job.nonce },
				);
				tx
			})
			.collect()
	};
	let flood = lane(&flood_keys, flood_jobs, flood_bytes);
	let probes = lane(&probe_keys, probe_jobs, probe_bytes);
	ensure!(state.len() == flood.len() + probes.len(), "two prepared transactions share a hash");
	let name = |part: &str| format!("substrate-{sequence}-{part}.json");
	let dir = &context.artifact_dir;
	Ok(json!({
		"flood": Artifact::write(dir, &name("flood"), &flood)?,
		"probes": Artifact::write(dir, &name("probes"), &probes)?,
		"state": Artifact::write(dir, &name("state"), &state)?,
		"identity": identity(&client).await?,
		"summary": {
			"call": format!("{pallet}.{call}"),
			"flood": flood.len(),
			"probes": probes.len(),
			"floodSigners": flood_keys.len(),
			"probeSigners": probe_keys.len(),
			"preparedAt": builder.block_number(),
			"mortality": spec.mortality.unwrap_or(0),
			"amount": (amount > 0).then(|| amount.to_string()),
		},
	}))
}

impl Substrate {
	async fn client_for(&mut self, target: &str) -> Result<Client> {
		if self.check_client.as_ref().is_none_or(|(url, _)| url != target) {
			self.check_client = Some((target.to_owned(), connect(target).await?));
		}
		Ok(self.check_client.as_ref().expect("set above").1.clone())
	}

	async fn check_state(&mut self, inputs: &Value, context: &Context) -> Result<Value> {
		let input: StateCheckInput = serde_json::from_value(inputs.clone())?;
		let state: polkameter_plugin_sdk::Artifact = serde_json::from_value(input.state.clone())
			.context("input state must be the state artifact of a substrate prepare step")?;
		if self
			.check_state
			.as_ref()
			.is_none_or(|(path, hash, _)| path != &state.path || hash != &state.blake2)
		{
			let map: HashMap<String, Signed> = state.read(&context.artifact_dir)?;
			self.check_state = Some((state.path.clone(), state.blake2.clone(), map));
		}
		let at: [u8; 32] = hex::decode(strip_0x(&input.at))?
			.try_into()
			.map_err(|_| anyhow::anyhow!("input at is not a block hash"))?;
		let client = self.client_for(&input.target).await?;
		let result = self.count_missing(&client, &input, at).await;
		match result {
			Err(error) if error.is::<ChainError>() => {
				// A long run can drop the cached connection. The read is at a fixed block, so one
				// retry on a new connection is safe.
				self.check_client = None;
				let client = self.client_for(&input.target).await?;
				self.count_missing(&client, &input, at).await
			},
			other => other,
		}
	}

	async fn count_missing(
		&self,
		client: &Client,
		input: &StateCheckInput,
		at: [u8; 32],
	) -> Result<Value> {
		let state = &self.check_state.as_ref().expect("loaded by check_state").2;
		let block = client.at(at).await?;
		let mut nonces: HashMap<&str, u64> = HashMap::new();
		let mut missing = Vec::new();
		for hash in &input.hashes {
			let signed = state
				.get(strip_0x(hash))
				.with_context(|| format!("transaction {hash} is not in the prepared state"))?;
			let nonce = match nonces.get(signed.signer.as_str()) {
				Some(nonce) => *nonce,
				None => {
					let account = polkameter_plugin_sdk::chain::parse_account(&signed.signer)?;
					let nonce = account_at(&block, account).await?.nonce;
					nonces.insert(&signed.signer, nonce);
					nonce
				},
			};
			if nonce <= signed.nonce {
				missing.push(format!("{hash} (nonce {}, account at {nonce})", signed.nonce));
			}
		}
		let detail = if missing.is_empty() {
			format!("every sampled transaction's signer nonce is past it at block {}", hex0x(at))
		} else {
			format!(
				"{} sampled transactions left their signer's nonce unchanged at block {}: {}",
				missing.len(),
				hex0x(at),
				missing.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
			)
		};
		Ok(json!({"checked": input.hashes.len(), "missing": missing.len(), "detail": detail}))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_manifest_names_every_operation() {
		let manifest = manifest();
		assert_eq!(manifest.id, "substrate");
		assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
		let names: Vec<&str> = manifest.operations.keys().map(String::as_str).collect();
		assert_eq!(
			names,
			["check-state", "fund-accounts", "preflight", "prepare-remarks", "prepare-transfers"]
		);
		assert!(manifest.operations["preflight"].read_only);
		assert!(manifest.operations["check-state"].read_only);
		assert!(!manifest.operations["fund-accounts"].read_only);
		let remarks = &manifest.operations["prepare-remarks"];
		assert!(!remarks.inputs["count"].optional);
		assert!(remarks.inputs["payload-bytes"].optional);
		assert!(!remarks.inputs.contains_key("amount"));
		assert!(manifest.operations["prepare-transfers"].inputs["dest"].optional);
	}

	#[test]
	fn refusals_name_the_fix() {
		let invalid = |raw: &str| ChainError::Invalid {
			what: "first flood transaction".into(),
			kind: polkameter_plugin_sdk::chain::Refusal::Invalid,
			code: 0,
			raw: raw.into(),
		};
		assert!(describe(&invalid("010001")).ends_with("fund it with substrate.fund-accounts"));
		assert!(describe(&invalid("010003")).contains("the nonce is already used"));
		let other = describe(&invalid("01000704"));
		assert!(other.starts_with("first flood transaction is not valid"), "{other}");
		assert!(!other.contains("fund it"));
	}

	#[test]
	fn payloads_are_unique_and_sized() {
		let a = payload("run-1", "flood", 0, 100);
		assert_eq!(a.len(), 100);
		assert_ne!(a, payload("run-1", "flood", 1, 100));
		assert_ne!(a, payload("run-1", "probe", 0, 100));
		assert_ne!(a, payload("run-2", "flood", 0, 100));
		assert_eq!(payload("r", "flood", 0, 1).len(), 1);
		assert_eq!(&payload("r", "flood", 3, 64)[..32], &payload("r", "flood", 3, 32)[..]);
	}
}
