//! The SDK for Polkameter plugins.
//!
//! The crate root is the versioned JSON-line protocol a plugin binary speaks ([`Plugin`],
//! [`serve`], [`Manifest`] and the message types). Two optional features add the libraries a
//! plugin may use:
//!
//! | feature     | module         | what it gives                                                  |
//! | ----------- | -------------- | -------------------------------------------------------------- |
//! | (default)   | crate root     | the protocol: manifest, requests, responses, `serve`           |
//! | `chain`     | `chain`        | a node over RPC: typed reads, events, submission, keypairs     |
//! | `observers` | `observers`    | run files, metric registry, block walker, series, checks       |
//!
//! `observers` implies `chain`. Without features the crate does not depend on subxt.
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "chain")]
pub mod chain;
#[cfg(feature = "observers")]
pub mod observers;

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
	collections::BTreeMap,
	future::Future,
	path::{Path, PathBuf},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const PROTOCOL: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// Lowercase hex BLAKE2-256 digest: the checksum format of artifacts, transactions and plugins.
pub fn blake2_hex(bytes: &[u8]) -> String {
	hex::encode(sp_crypto_hashing::blake2_256(bytes))
}
/// Hex text without its optional `0x` prefix.
pub fn strip_0x(text: &str) -> &str {
	text.trim_start_matches("0x")
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Schema {
	String,
	Number,
	Integer,
	Boolean,
	Array { items: Box<Schema> },
	Object { fields: BTreeMap<String, Field> },
	Json,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Field {
	pub schema: Schema,
	#[serde(default)]
	pub optional: bool,
}
impl From<Schema> for Field {
	fn from(schema: Schema) -> Self {
		Self { schema, optional: false }
	}
}
impl Schema {
	pub fn validate(&self, value: &Value) -> Result<()> {
		let accepted = match self {
			Self::Json => return Ok(()),
			Self::Array { items } => {
				let array = value.as_array().ok_or_else(|| anyhow::anyhow!("expected array"))?;
				return array.iter().try_for_each(|item| items.validate(item));
			},
			Self::Object { fields } => return validate_object(fields, value),
			Self::String => value.is_string(),
			Self::Number => value.is_number(),
			Self::Integer => value.is_i64() || value.is_u64(),
			Self::Boolean => value.is_boolean(),
		};
		ensure!(accepted, "value does not match {self:?}");
		Ok(())
	}
}
fn validate_object(fields: &BTreeMap<String, Field>, value: &Value) -> Result<()> {
	let object = value.as_object().ok_or_else(|| anyhow::anyhow!("expected object"))?;
	for (key, field) in fields {
		match object.get(key) {
			Some(value) => field.schema.validate(value)?,
			None => ensure!(field.optional, "missing field {key}"),
		}
	}
	for key in object.keys() {
		ensure!(fields.contains_key(key), "unknown field {key}");
	}
	Ok(())
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
	pub description: String,
	pub inputs: BTreeMap<String, Field>,
	pub outputs: BTreeMap<String, Field>,
	/// Read-only operations may be used in live preflight.
	#[serde(default)]
	pub read_only: bool,
}
impl Operation {
	/// A state-changing operation over the given `(name, schema)` fields.
	pub fn new(description: &str, inputs: &[(&str, Schema)], outputs: &[(&str, Schema)]) -> Self {
		Self {
			description: description.into(),
			inputs: fields(inputs),
			outputs: fields(outputs),
			read_only: false,
		}
	}
	/// Marks the operation as safe in live preflight.
	pub fn read_only(mut self) -> Self {
		self.read_only = true;
		self
	}
	pub fn validate_inputs(&self, value: &Value) -> Result<()> {
		validate_object(&self.inputs, value)
	}
	pub fn validate_outputs(&self, value: &Value) -> Result<()> {
		validate_object(&self.outputs, value)
	}
}
fn fields(fields: &[(&str, Schema)]) -> BTreeMap<String, Field> {
	fields
		.iter()
		.map(|(name, schema)| ((*name).into(), Field::from(schema.clone())))
		.collect()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
	pub id: String,
	pub version: String,
	pub protocol: u32,
	pub operations: BTreeMap<String, Operation>,
	#[serde(default)]
	pub requirements: Vec<Requirement>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
	pub kind: String,
	pub target: String,
	pub name: String,
	pub required: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
	pub run_id: String,
	pub artifact_dir: PathBuf,
	pub user: Option<u32>,
	pub iteration: Option<u32>,
}
impl Context {
	/// Context for host work outside any workflow user or iteration.
	pub fn host(run_id: impl Into<String>, artifact_dir: impl Into<PathBuf>) -> Self {
		Self {
			run_id: run_id.into(),
			artifact_dir: artifact_dir.into(),
			user: None,
			iteration: None,
		}
	}
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
	pub protocol: u32,
	pub id: u64,
	pub operation: String,
	#[serde(default)]
	pub inputs: Value,
	pub context: Context,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
	pub protocol: u32,
	pub id: u64,
	pub result: Option<Value>,
	pub error: Option<String>,
}
impl Response {
	fn ok(id: u64, result: Value) -> Self {
		Self { protocol: PROTOCOL, id, result: Some(result), error: None }
	}
	fn err(id: u64, error: impl Into<String>) -> Self {
		Self { protocol: PROTOCOL, id, result: None, error: Some(error.into()) }
	}
	/// Checks the envelope against the request it answers and returns the result.
	pub fn into_result(self, id: u64) -> Result<Value> {
		ensure!(self.protocol == PROTOCOL && self.id == id, "plugin protocol/request ID mismatch");
		match (self.result, self.error) {
			(Some(result), None) => Ok(result),
			(None, Some(error)) => bail!("plugin operation failed: {error}"),
			_ => bail!("invalid plugin response"),
		}
	}
}
/// One process is retained for the run. The host serializes access to a plugin;
/// user and iteration state must be keyed by the supplied context.
pub trait Plugin {
	fn manifest(&self) -> Manifest;
	fn invoke(
		&mut self,
		operation: &str,
		inputs: Value,
		context: &Context,
	) -> impl Future<Output = Result<Value>> + Send;
}

pub async fn serve(mut plugin: impl Plugin) -> Result<()> {
	let mut input = BufReader::new(tokio::io::stdin());
	let mut output = tokio::io::stdout();
	while let Some(line) = read_line(&mut input).await? {
		let request: Request = serde_json::from_str(&line)?;
		let shutdown = request.operation == "$shutdown";
		let result = async {
			ensure!(request.protocol == PROTOCOL, "unsupported protocol");
			let manifest = plugin.manifest();
			if request.operation == "$describe" {
				return Ok(serde_json::to_value(manifest)?);
			}
			if shutdown {
				return Ok(serde_json::json!({}));
			}
			let operation = manifest
				.operations
				.get(&request.operation)
				.ok_or_else(|| anyhow::anyhow!("unknown operation {}", request.operation))?;
			operation.validate_inputs(&request.inputs)?;
			let result =
				plugin.invoke(&request.operation, request.inputs, &request.context).await?;
			operation.validate_outputs(&result)?;
			Ok(result)
		}
		.await;
		let response = result.map_or_else(
			|error| Response::err(request.id, error.to_string()),
			|result| Response::ok(request.id, result),
		);
		let encoded = serde_json::to_vec(&response)?;
		ensure!(
			encoded.len() <= MAX_MESSAGE_BYTES,
			"response exceeds protocol limit; use an artifact"
		);
		output.write_all(&encoded).await?;
		output.write_all(b"\n").await?;
		output.flush().await?;
		if shutdown {
			break;
		}
	}
	Ok(())
}
/// Bounded line reader, including when a subprocess never sends a newline.
pub async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
	reader: &mut R,
) -> Result<Option<String>> {
	let mut bytes = Vec::new();
	loop {
		let buf = reader.fill_buf().await?;
		if buf.is_empty() {
			ensure!(bytes.is_empty(), "truncated protocol message");
			return Ok(None);
		}
		let newline = buf.iter().position(|b| *b == b'\n');
		let len = newline.map_or(buf.len(), |i| i + 1);
		ensure!(bytes.len() + len <= MAX_MESSAGE_BYTES, "protocol message too large");
		bytes.extend_from_slice(&buf[..len]);
		reader.consume(len);
		if newline.is_some() {
			return Ok(Some(String::from_utf8(bytes)?));
		}
	}
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
	pub version: u32,
	pub path: PathBuf,
	pub blake2: String,
}
impl Artifact {
	pub fn write(directory: &Path, name: &str, value: &impl Serialize) -> Result<Self> {
		ensure!(!name.contains('/') && !name.contains('\\'), "invalid artifact name");
		std::fs::create_dir_all(directory)?;
		let bytes = serde_json::to_vec(value)?;
		std::fs::write(directory.join(name), &bytes)?;
		Ok(Self { version: 1, path: directory.join(name), blake2: blake2_hex(&bytes) })
	}
	pub fn read<T: serde::de::DeserializeOwned>(&self, directory: &Path) -> Result<T> {
		ensure!(self.version == 1, "unsupported artifact version");
		let path = self.path.canonicalize()?;
		ensure!(
			path.starts_with(directory.canonicalize()?),
			"artifact is outside the run directory"
		);
		ensure!(std::fs::metadata(&path)?.len() <= 256 * 1024 * 1024, "artifact too large");
		let bytes = std::fs::read(path)?;
		ensure!(blake2_hex(&bytes) == self.blake2, "artifact checksum mismatch");
		Ok(serde_json::from_slice(&bytes)?)
	}
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedTx {
	pub bytes: String,
	pub hash: String,
	#[serde(default)]
	pub metadata: Value,
}
impl PreparedTx {
	pub fn new(bytes: &[u8], metadata: Value) -> Self {
		Self { bytes: hex::encode(bytes), hash: blake2_hex(bytes), metadata }
	}
	pub fn decode(&self) -> Result<Vec<u8>> {
		let bytes = hex::decode(strip_0x(&self.bytes))?;
		ensure!(!bytes.is_empty(), "empty extrinsic");
		if blake2_hex(&bytes) != strip_0x(&self.hash) {
			bail!("transaction hash mismatch");
		}
		Ok(bytes)
	}
}
/// Input of a load plan's `state-check` operation: a sample of the transactions the run finalized,
/// with the plan's state reference. `hashes` and `at` are lowercase hex without `0x`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateCheckInput {
	pub target: String,
	pub state: Value,
	pub hashes: Vec<String>,
	pub at: String,
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	fn object(fields: &[(&str, Schema, bool)]) -> Schema {
		Schema::Object {
			fields: fields
				.iter()
				.map(|(name, schema, optional)| {
					((*name).into(), Field { schema: schema.clone(), optional: *optional })
				})
				.collect(),
		}
	}

	#[test]
	fn scalar_schemas_accept_their_type_only() {
		assert!(Schema::String.validate(&json!("a")).is_ok());
		assert!(Schema::String.validate(&json!(1)).is_err());
		assert!(Schema::Number.validate(&json!(1.5)).is_ok());
		assert!(Schema::Number.validate(&json!("1")).is_err());
		assert!(Schema::Integer.validate(&json!(-3)).is_ok());
		assert!(Schema::Integer.validate(&json!(u64::MAX)).is_ok());
		assert!(Schema::Integer.validate(&json!(1.5)).is_err());
		assert!(Schema::Boolean.validate(&json!(true)).is_ok());
		assert!(Schema::Boolean.validate(&json!(0)).is_err());
		for value in [json!(null), json!([1]), json!({"a": 1}), json!("x")] {
			assert!(Schema::Json.validate(&value).is_ok());
		}
		let error = Schema::String.validate(&json!(1)).unwrap_err().to_string();
		assert_eq!(error, "value does not match String");
	}

	#[test]
	fn arrays_validate_every_item() {
		let schema = Schema::Array { items: Box::new(Schema::Integer) };
		assert!(schema.validate(&json!([])).is_ok());
		assert!(schema.validate(&json!([1, 2, 3])).is_ok());
		assert!(schema.validate(&json!([1, "2"])).is_err());
		assert_eq!(schema.validate(&json!({})).unwrap_err().to_string(), "expected array");
	}

	#[test]
	fn objects_check_required_optional_and_unknown_fields() {
		let schema = object(&[("name", Schema::String, false), ("age", Schema::Integer, true)]);
		assert!(schema.validate(&json!({"name": "a"})).is_ok());
		assert!(schema.validate(&json!({"name": "a", "age": 3})).is_ok());
		let missing = schema.validate(&json!({"age": 3})).unwrap_err().to_string();
		assert_eq!(missing, "missing field name");
		let unknown = schema.validate(&json!({"name": "a", "x": 1})).unwrap_err().to_string();
		assert_eq!(unknown, "unknown field x");
		assert!(schema.validate(&json!({"name": "a", "age": "3"})).is_err());
		assert_eq!(schema.validate(&json!([])).unwrap_err().to_string(), "expected object");
		let nested = object(&[("inner", schema, false)]);
		assert!(nested.validate(&json!({"inner": {"name": "a"}})).is_ok());
		assert!(nested.validate(&json!({"inner": {}})).is_err());
	}

	#[test]
	fn operation_validates_inputs_and_outputs() {
		let op =
			Operation::new("greet", &[("name", Schema::String)], &[("greeting", Schema::String)]);
		assert!(!op.read_only);
		assert!(op.clone().read_only().read_only);
		assert!(op.validate_inputs(&json!({"name": "a"})).is_ok());
		assert!(op.validate_inputs(&json!({"name": 1})).is_err());
		assert!(op.validate_outputs(&json!({"greeting": "hi"})).is_ok());
		assert!(op.validate_outputs(&json!({})).is_err());
	}

	#[test]
	fn manifest_round_trips_and_rejects_unknown_fields() {
		let manifest = Manifest {
			id: "hello".into(),
			version: "0.1.0".into(),
			protocol: PROTOCOL,
			operations: BTreeMap::from([(
				"greet".into(),
				Operation::new(
					"Greets",
					&[("names", Schema::Array { items: Box::new(Schema::String) })],
					&[("greeting", Schema::String)],
				)
				.read_only(),
			)]),
			requirements: vec![Requirement {
				kind: "pallet".into(),
				target: "chain".into(),
				name: "People".into(),
				required: true,
			}],
		};
		let encoded = serde_json::to_value(&manifest).unwrap();
		assert_eq!(encoded["operations"]["greet"]["inputs"]["names"]["schema"]["type"], "array");
		assert_eq!(encoded["operations"]["greet"]["read_only"], true);
		let decoded: Manifest = serde_json::from_value(encoded.clone()).unwrap();
		assert_eq!(serde_json::to_value(&decoded).unwrap(), encoded);
		assert_eq!(decoded.operations["greet"].inputs, manifest.operations["greet"].inputs);

		// Defaults: no requirements, not read-only, a field is required.
		let minimal: Manifest = serde_json::from_value(json!({
			"id": "x", "version": "1", "protocol": 1,
			"operations": {"op": {
				"description": "", "inputs": {"a": {"schema": {"type": "json"}}}, "outputs": {}
			}}
		}))
		.unwrap();
		assert!(minimal.requirements.is_empty());
		assert!(!minimal.operations["op"].read_only);
		assert!(!minimal.operations["op"].inputs["a"].optional);

		let mut extra = encoded;
		extra["unexpected"] = json!(1);
		assert!(serde_json::from_value::<Manifest>(extra).is_err());
		assert!(serde_json::from_value::<Schema>(json!({"type": "date"})).is_err());
	}

	#[test]
	fn response_matches_its_request() {
		let ok = Response::ok(7, json!({"a": 1}));
		assert_eq!(ok.into_result(7).unwrap(), json!({"a": 1}));
		assert!(Response::ok(7, json!(1)).into_result(8).is_err());
		let error = Response::err(7, "boom").into_result(7).unwrap_err().to_string();
		assert_eq!(error, "plugin operation failed: boom");
		let both =
			Response { protocol: PROTOCOL, id: 1, result: Some(json!(1)), error: Some("x".into()) };
		assert!(both.into_result(1).is_err());
		let newer = Response { protocol: PROTOCOL + 1, id: 1, result: Some(json!(1)), error: None };
		assert!(newer.into_result(1).is_err());
	}

	#[test]
	fn hex_helpers() {
		assert_eq!(strip_0x("0xab"), "ab");
		assert_eq!(strip_0x("ab"), "ab");
		assert_eq!(strip_0x(""), "");
		assert_eq!(
			blake2_hex(b""),
			"0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8"
		);
		let digest = blake2_hex(b"polkameter");
		assert_eq!(digest.len(), 64);
		assert!(digest.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
		assert_ne!(digest, blake2_hex(b"polkameter2"));
	}

	#[test]
	fn prepared_tx_checks_its_hash() {
		let tx = PreparedTx::new(&[1, 2, 3], json!({"lane": "a"}));
		assert_eq!(tx.bytes, "010203");
		assert_eq!(tx.decode().unwrap(), vec![1, 2, 3]);
		let prefixed =
			PreparedTx { bytes: "0x010203".into(), hash: format!("0x{}", tx.hash), ..tx.clone() };
		assert_eq!(prefixed.decode().unwrap(), vec![1, 2, 3]);
		let tampered = PreparedTx { bytes: "010204".into(), ..tx.clone() };
		assert_eq!(tampered.decode().unwrap_err().to_string(), "transaction hash mismatch");
		let empty =
			PreparedTx { bytes: String::new(), hash: blake2_hex(&[]), metadata: Value::Null };
		assert!(empty.decode().is_err());
		let not_hex = PreparedTx { bytes: "zz".into(), ..tx };
		assert!(not_hex.decode().is_err());
	}

	#[test]
	fn artifacts_round_trip_and_detect_tampering() {
		let scratch = |name: &str| {
			std::env::temp_dir().join(format!("polkameter-sdk-{name}-{}", std::process::id()))
		};
		let dir = scratch("artifact");
		let artifact = Artifact::write(&dir, "data.json", &json!({"n": [1, 2]})).unwrap();
		assert_eq!(artifact.version, 1);
		let read: Value = artifact.read(&dir).unwrap();
		assert_eq!(read, json!({"n": [1, 2]}));
		assert!(Artifact::write(&dir, "a/b.json", &1).is_err());
		let elsewhere = scratch("elsewhere");
		std::fs::create_dir_all(&elsewhere).unwrap();
		assert!(artifact.read::<Value>(&elsewhere).is_err());
		std::fs::write(dir.join("data.json"), b"{\"n\":[1,3]}").unwrap();
		let error = artifact.read::<Value>(&dir).unwrap_err().to_string();
		assert_eq!(error, "artifact checksum mismatch");
		std::fs::remove_dir_all(dir).unwrap();
		std::fs::remove_dir_all(elsewhere).unwrap();
	}

	async fn lines(data: &[u8], capacity: usize) -> Result<Vec<String>> {
		let mut reader = BufReader::with_capacity(capacity, data);
		let mut lines = Vec::new();
		while let Some(line) = read_line(&mut reader).await? {
			lines.push(line);
		}
		Ok(lines)
	}

	#[tokio::test]
	async fn read_line_splits_lines_across_buffer_chunks() {
		let got = lines(b"{\"a\":1}\n\n{\"b\":2}\n", 3).await.unwrap();
		assert_eq!(got, ["{\"a\":1}\n", "\n", "{\"b\":2}\n"]);
		assert!(lines(b"", 8).await.unwrap().is_empty());
	}

	#[tokio::test]
	async fn read_line_rejects_truncated_and_invalid_messages() {
		let error = lines(b"{\"a\":1}\n{\"b\"", 4).await.unwrap_err().to_string();
		assert_eq!(error, "truncated protocol message");
		assert!(lines(b"\xff\xfe\n", 4).await.is_err());
	}

	#[tokio::test]
	async fn read_line_enforces_the_message_limit() {
		// The limit counts the newline: exactly MAX_MESSAGE_BYTES is accepted.
		let mut at_limit = vec![b'a'; MAX_MESSAGE_BYTES - 1];
		at_limit.push(b'\n');
		let got = lines(&at_limit, 64 * 1024).await.unwrap();
		assert_eq!(got.len(), 1);
		assert_eq!(got[0].len(), MAX_MESSAGE_BYTES);

		let mut over = vec![b'a'; MAX_MESSAGE_BYTES];
		over.push(b'\n');
		let error = lines(&over, 64 * 1024).await.unwrap_err().to_string();
		assert_eq!(error, "protocol message too large");

		// A peer that never sends a newline is cut off at the limit, not buffered forever.
		let endless = vec![b'a'; MAX_MESSAGE_BYTES + 1];
		let error = lines(&endless, 64 * 1024).await.unwrap_err().to_string();
		assert_eq!(error, "protocol message too large");
	}
}
