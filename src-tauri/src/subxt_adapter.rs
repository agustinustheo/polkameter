use std::{str::FromStr, time::Duration};

use subxt::{dynamic, tx::DynamicPayload, OnlineClient, PolkadotConfig};
use subxt_signer::{sr25519::Keypair, SecretUri};

use crate::scenario::{
	required_signer_count, signer_suri, CompletionBoundary, ScenarioDocument, TransactionSampler,
};

pub struct SubxtRuntimeAdapter {
	client: OnlineClient<PolkadotConfig>,
}

pub struct Submission {
	pub message: String,
	pub extrinsic_hash: Option<String>,
	pub block_hash: Option<String>,
}

impl SubxtRuntimeAdapter {
	pub async fn connect(endpoint: &str) -> Result<Self, String> {
		let client = OnlineClient::<PolkadotConfig>::from_insecure_url(endpoint)
			.await
			.map_err(|error| format!("could not connect to {endpoint}: {error}"))?;
		Ok(Self { client })
	}

	pub async fn ensure_ready(
		&self,
		document: &ScenarioDocument,
		run_id: &str,
	) -> Result<(), String> {
		let at_block = self
			.client
			.at_current_block()
			.await
			.map_err(|error| format!("could not read latest block before arm: {error}"))?;
		let storage = at_block.storage();
		for index in 0..required_signer_count(document) {
			let signer = derive_signer(document, index, run_id)?;
			let account = signer.public_key().to_account_id();
			let account_storage = dynamic::storage::<_, dynamic::Value>("System", "Account");
			storage
				.try_fetch(
					account_storage,
					vec![dynamic::Value::from_bytes(
						<subxt::utils::AccountId32 as AsRef<[u8]>>::as_ref(&account),
					)],
				)
				.await
				.map_err(|error| {
					format!("could not read signer {index} balance before arm: {error}")
				})?
				.ok_or_else(|| format!("signer {index} has no on-chain balance record"))?;
			at_block.tx().account_nonce(&account).await.map_err(|error| {
				format!("could not read signer {index} nonce before arm: {error}")
			})?;
		}
		Ok(())
	}

	pub async fn fund_derived_signers(
		&self,
		document: &ScenarioDocument,
		run_id: &str,
	) -> Result<u32, String> {
		let Some(funding) = &document.signer_source.funding else {
			return Ok(0);
		};
		let amount = funding
			.amount
			.parse::<u128>()
			.map_err(|error| format!("invalid funding amount: {error}"))?;
		let funder = base_signer(document)?;
		let recipients = (0..required_signer_count(document))
			.filter(|index| {
				signer_suri(document, *index, run_id) != document.signer_source.base_suri
			})
			.map(|index| {
				derive_signer(document, index, run_id)
					.map(|signer| (index, signer.public_key().to_account_id()))
			})
			.collect::<Result<Vec<_>, _>>()?;
		let mut funded = 0;
		for batch in recipients.chunks(funding.batch_size as usize) {
			let first_index = batch.first().map(|(index, _)| *index).unwrap_or_default();
			let last_index = batch.last().map(|(index, _)| *index).unwrap_or_default();
			let calls = batch
				.iter()
				.map(|(_, recipient)| {
					funding_transfer_call(recipient, amount).map(|call| call.into_value())
				})
				.collect::<Result<Vec<_>, _>>()?;
			let call =
				dynamic::tx("Utility", "batch_all", vec![dynamic::Value::unnamed_composite(calls)]);
			let finalize = async {
				let mut tx = self.client.tx().await.map_err(|error| {
					format!("could not prepare funding batch {first_index}..={last_index}: {error}")
				})?;
				tx.sign_and_submit_then_watch_default(&call, &funder)
					.await
					.map_err(|error| {
						format!(
							"could not submit funding batch {first_index}..={last_index}: {error}"
						)
					})?
					.wait_for_finalized_success()
					.await
					.map_err(|error| {
						format!("funding batch {first_index}..={last_index} did not finalize successfully: {error}")
					})
			};
			tokio::time::timeout(Duration::from_millis(funding.finality_timeout_ms), finalize)
				.await
				.map_err(|_| {
					format!(
						"funding batch {first_index}..={last_index} exceeded the {} ms finality deadline",
						funding.finality_timeout_ms
					)
				})??;
			funded += batch.len() as u32;
		}
		Ok(funded)
	}

	pub async fn submit(
		&self,
		document: &ScenarioDocument,
		signer_index: u32,
		run_id: &str,
		sampler: &TransactionSampler,
	) -> Result<Submission, String> {
		let signer = derive_signer(document, signer_index, run_id)?;
		let args = crate::preflight::arguments_to_composite_for_runner(&sampler.arguments)?;
		let call = dynamic::tx(&sampler.pallet, &sampler.call, args);
		let wait = async {
			match sampler.completion {
				CompletionBoundary::Submitted => {
					let mut tx = self.client.tx().await.map_err(|error| error.to_string())?;
					tx.sign_and_submit_default(&call, &signer)
						.await
						.map_err(|error| error.to_string())
						.map(|hash| Submission {
							message: format!("submitted {hash:#x}"),
							extrinsic_hash: Some(format!("{hash:#x}")),
							block_hash: None,
						})
				},
				CompletionBoundary::InBlock => {
					let mut tx = self.client.tx().await.map_err(|error| error.to_string())?;
					let mut progress = tx
						.sign_and_submit_then_watch_default(&call, &signer)
						.await
						.map_err(|error| error.to_string())?;
					loop {
						match progress
							.next()
							.await
							.ok_or_else(|| "transaction subscription closed".to_string())?
							.map_err(|error| error.to_string())?
						{
							subxt::tx::TransactionStatus::InBestBlock(block) => {
								let extrinsic_hash = format!("{:#x}", block.extrinsic_hash());
								let block_hash = format!("{:#x}", block.block_hash());
								break block
									.wait_for_success()
									.await
									.map_err(|error| error.to_string())
									.map(|_| Submission {
										message: "included in block".into(),
										extrinsic_hash: Some(extrinsic_hash),
										block_hash: Some(block_hash),
									});
							},
							subxt::tx::TransactionStatus::Error { message }
							| subxt::tx::TransactionStatus::Invalid { message }
							| subxt::tx::TransactionStatus::Dropped { message } => break Err(message),
							_ => {},
						}
					}
				},
				CompletionBoundary::Finalized => {
					let mut tx = self.client.tx().await.map_err(|error| error.to_string())?;
					let progress = tx
						.sign_and_submit_then_watch_default(&call, &signer)
						.await
						.map_err(|error| error.to_string())?;
					let block =
						progress.wait_for_finalized().await.map_err(|error| error.to_string())?;
					let extrinsic_hash = format!("{:#x}", block.extrinsic_hash());
					let block_hash = format!("{:#x}", block.block_hash());
					block.wait_for_success().await.map_err(|error| error.to_string()).map(|_| {
						Submission {
							message: "finalized".into(),
							extrinsic_hash: Some(extrinsic_hash),
							block_hash: Some(block_hash),
						}
					})
				},
			}
		};
		tokio::time::timeout(Duration::from_millis(sampler.finality_timeout_ms), wait)
			.await
			.map_err(|_| "FINALITY_TIMEOUT".to_string())?
			.map_err(|error| error.to_string())
	}
}

fn derive_signer(document: &ScenarioDocument, index: u32, run_id: &str) -> Result<Keypair, String> {
	let uri = SecretUri::from_str(&signer_suri(document, index, run_id))
		.map_err(|error| format!("invalid signer SURI: {error}"))?;
	Keypair::from_uri(&uri).map_err(|error| format!("could not derive signer: {error}"))
}

fn base_signer(document: &ScenarioDocument) -> Result<Keypair, String> {
	let uri = SecretUri::from_str(&document.signer_source.base_suri)
		.map_err(|error| format!("invalid base signer SURI: {error}"))?;
	Keypair::from_uri(&uri).map_err(|error| format!("could not derive base signer: {error}"))
}

fn funding_transfer_call(
	recipient: &subxt::utils::AccountId32,
	amount: u128,
) -> Result<DynamicPayload<subxt::ext::scale_value::Composite<()>>, String> {
	Ok(dynamic::tx(
		"Balances",
		"transfer_keep_alive",
		crate::preflight::arguments_to_composite_for_runner(&serde_json::json!({
			"dest": {
				"$variant": "Id",
				"value": {
					"$bytes": format!("0x{}", hex::encode(<subxt::utils::AccountId32 as AsRef<[u8]>>::as_ref(recipient)))
				}
			},
			"value": amount.to_string()
		}))?,
	))
}
