use std::str::FromStr;

use serde::Serialize;
use subxt::{
	dynamic::{self, Value},
	ext::scale_value::{Composite, ValueDef},
	OnlineClient, OnlineClientAtBlock, PolkadotConfig,
};
use subxt_signer::{sr25519::Keypair, SecretUri};

use crate::scenario::{signer_derivation_root, signer_suri, ScenarioDocument, TransactionSampler};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReport {
	pub run_id: String,
	pub signer_derivation_root: String,
	pub endpoint: String,
	pub genesis_hash: String,
	pub spec_version: u32,
	pub transaction_version: u32,
	pub metadata_hash: String,
	pub pallets: Vec<PalletSchema>,
	pub selected_calls: Vec<CallValidation>,
	pub derived_accounts: Vec<DerivedAccount>,
	pub readiness: Readiness,
	pub resolved_sample_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalletSchema {
	pub name: String,
	pub calls: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallValidation {
	pub group: String,
	pub label: String,
	pub pallet: String,
	pub call: String,
	pub fields: Vec<ArgumentField>,
	pub encodable: bool,
	pub encoded_call_bytes: Option<usize>,
	pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgumentField {
	pub name: Option<String>,
	pub type_id: u32,
	pub type_name: Option<String>,
	pub docs: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedAccount {
	pub index: u32,
	pub address: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Readiness {
	pub signer_source: String,
	pub balance_and_nonce: String,
	pub transaction_profile: String,
}

pub async fn preflight(
	document: &ScenarioDocument,
	run_id: &str,
) -> Result<PreflightReport, String> {
	let issues = document.validate();
	if !issues.is_empty() {
		return Err(format!("scenario is invalid: {}: {}", issues[0].field, issues[0].message));
	}

	let client = OnlineClient::<PolkadotConfig>::from_insecure_url(&document.chain.endpoint)
		.await
		.map_err(|error| format!("could not connect to {}: {error}", document.chain.endpoint))?;
	let at_block = client
		.at_current_block()
		.await
		.map_err(|error| format!("could not read the current block for preflight: {error}"))?;
	let metadata = at_block.metadata();
	let pallets = metadata
		.pallets()
		.map(|pallet| PalletSchema {
			name: pallet.name().into(),
			calls: pallet
				.call_variants()
				.unwrap_or_default()
				.iter()
				.map(|call| call.name.clone())
				.collect(),
		})
		.collect();
	let mut selected_calls = Vec::new();
	for group in &document.thread_groups {
		for sampler in &group.samplers {
			selected_calls.push(validate_sampler(&at_block, &group.name, sampler));
		}
	}

	let derived_accounts = derive_accounts(document, run_id, 8)?;
	let metadata_hash = format!("0x{}", hex::encode(metadata.hasher().hash()));
	let genesis_hash = format!("{:#x}", client.genesis_hash());
	let resolved_sample_count = document
		.thread_groups
		.iter()
		.map(|group| u64::from(group.users) * group.samplers.len() as u64)
		.sum();

	Ok(PreflightReport {
		run_id: run_id.into(),
		signer_derivation_root: signer_derivation_root(document, run_id),
		endpoint: document.chain.endpoint.clone(),
		genesis_hash,
		spec_version: at_block.spec_version(),
		transaction_version: at_block.transaction_version(),
		metadata_hash,
		pallets,
		selected_calls,
		derived_accounts,
		readiness: Readiness {
			signer_source: "development SURI derived locally; secret retained only in memory"
				.into(),
			balance_and_nonce:
				"checked immediately before arm/start; preflight performs no account mutation"
					.into(),
			transaction_profile: format!("{:?}", document.chain.transaction_profile),
		},
		resolved_sample_count,
	})
}

fn validate_sampler(
	client: &OnlineClientAtBlock<PolkadotConfig>,
	group_name: &str,
	sampler: &TransactionSampler,
) -> CallValidation {
	let fields = client
		.metadata()
		.pallet_by_name(&sampler.pallet)
		.and_then(|pallet| pallet.call_variant_by_name(&sampler.call))
		.map(|call| {
			call.fields
				.iter()
				.map(|field| ArgumentField {
					name: field.name.clone(),
					type_id: field.ty.id,
					type_name: field.type_name.clone(),
					docs: field.docs.clone(),
				})
				.collect()
		})
		.unwrap_or_default();
	let encoded = arguments_to_composite_for_runner(&sampler.arguments).and_then(|arguments| {
		client
			.tx()
			.call_data(&dynamic::tx(&sampler.pallet, &sampler.call, arguments))
			.map_err(|error| error.to_string())
	});
	let (encodable, encoded_call_bytes, error) = match encoded {
		Ok(bytes) => (true, Some(bytes.len()), None),
		Err(error) => (false, None, Some(error)),
	};
	CallValidation {
		group: group_name.into(),
		label: sampler.label.clone(),
		pallet: sampler.pallet.clone(),
		call: sampler.call.clone(),
		fields,
		encodable,
		encoded_call_bytes,
		error,
	}
}

fn derive_accounts(
	document: &ScenarioDocument,
	run_id: &str,
	limit: u32,
) -> Result<Vec<DerivedAccount>, String> {
	let users = document.thread_groups.iter().map(|group| group.users).sum::<u32>().min(limit);
	(0..users)
		.map(|index| {
			let suri = signer_suri(document, index, run_id);
			let secret_uri = SecretUri::from_str(&suri)
				.map_err(|error| format!("invalid signer source: {error}"))?;
			let keypair = Keypair::from_uri(&secret_uri)
				.map_err(|error| format!("could not derive signer: {error}"))?;
			Ok(DerivedAccount { index, address: keypair.public_key().to_account_id().to_string() })
		})
		.collect()
}

pub fn arguments_to_composite_for_runner(
	value: &serde_json::Value,
) -> Result<Composite<()>, String> {
	match json_to_value(value)? {
		Value { value: ValueDef::Composite(composite), .. } => Ok(composite),
		_ => Err("transaction arguments must be a JSON object or array".into()),
	}
}

fn json_to_value(value: &serde_json::Value) -> Result<Value<()>, String> {
	match value {
		serde_json::Value::Null => Ok(Value::unnamed_variant("None", [])),
		serde_json::Value::Bool(value) => Ok(Value::bool(*value)),
		serde_json::Value::Number(value) => {
			if let Some(value) = value.as_u64() {
				Ok(Value::u128(u128::from(value)))
			} else if let Some(value) = value.as_i64() {
				Ok(Value::i128(i128::from(value)))
			} else {
				Err("floating point values are not SCALE encodable; use an integer or decimal string".into())
			}
		},
		serde_json::Value::String(value) => match value.parse::<u128>() {
			Ok(value) => Ok(Value::u128(value)),
			Err(_) => Ok(Value::string(value)),
		},
		serde_json::Value::Array(values) => values
			.iter()
			.map(json_to_value)
			.collect::<Result<Vec<_>, _>>()
			.map(Value::unnamed_composite),
		serde_json::Value::Object(values) => {
			if let Some(variant) = values.get("$variant").and_then(serde_json::Value::as_str) {
				let fields = values
					.get("value")
					.map(json_to_value)
					.transpose()?
					.into_iter()
					.collect::<Vec<_>>();
				return Ok(Value::unnamed_variant(variant, fields));
			}
			if let Some(bytes) = values.get("$bytes").and_then(serde_json::Value::as_str) {
				let bytes = bytes.strip_prefix("0x").ok_or("$bytes must use a 0x prefix")?;
				return hex::decode(bytes)
					.map(Value::from_bytes)
					.map_err(|error| error.to_string());
			}
			values
				.iter()
				.map(|(name, value)| Ok((name.as_str(), json_to_value(value)?)))
				.collect::<Result<Vec<_>, String>>()
				.map(Value::named_composite)
		},
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn dynamic_json_accepts_variant_and_byte_markers() {
		let arguments = serde_json::json!({
			"dest": { "$variant": "Id", "value": { "$bytes": "0x0102" } },
			"value": "1000000000"
		});
		assert!(arguments_to_composite_for_runner(&arguments).is_ok());
	}
}
