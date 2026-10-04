//! Versioned protocol for independently installed Polkameter components.
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
		let valid = match self {
			Self::String => value.is_string(),
			Self::Number => value.is_number(),
			Self::Integer => value.is_i64() || value.is_u64(),
			Self::Boolean => value.is_boolean(),
			Self::Json => true,
			Self::Array { items } => {
				let array = value.as_array().ok_or_else(|| anyhow::anyhow!("expected array"))?;
				for item in array {
					items.validate(item)?;
				}
				true
			},
			Self::Object { fields } => {
				let object = value.as_object().ok_or_else(|| anyhow::anyhow!("expected object"))?;
				for (key, field) in fields {
					if let Some(value) = object.get(key) {
						field.schema.validate(value)?;
					} else {
						ensure!(field.optional, "missing field {key}");
					}
				}
				for key in object.keys() {
					ensure!(fields.contains_key(key), "unknown field {key}");
				}
				true
			},
		};
		ensure!(valid, "value does not match {self:?}");
		Ok(())
	}
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
	pub fn validate_inputs(&self, value: &Value) -> Result<()> {
		Schema::Object { fields: self.inputs.clone() }.validate(value)
	}
	pub fn validate_outputs(&self, value: &Value) -> Result<()> {
		Schema::Object { fields: self.outputs.clone() }.validate(value)
	}
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
		let response = match result {
			Ok(result) => {
				Response { protocol: PROTOCOL, id: request.id, result: Some(result), error: None }
			},
			Err(error) => Response {
				protocol: PROTOCOL,
				id: request.id,
				result: None,
				error: Some(error.to_string()),
			},
		};
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
		Ok(Self {
			version: 1,
			path: directory.join(name),
			blake2: hex::encode(sp_crypto_hashing::blake2_256(&bytes)),
		})
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
		ensure!(
			hex::encode(sp_crypto_hashing::blake2_256(&bytes)) == self.blake2,
			"artifact checksum mismatch"
		);
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
		Self {
			bytes: hex::encode(bytes),
			hash: hex::encode(sp_crypto_hashing::blake2_256(bytes)),
			metadata,
		}
	}
	pub fn decode(&self) -> Result<Vec<u8>> {
		let bytes = hex::decode(self.bytes.trim_start_matches("0x"))?;
		ensure!(!bytes.is_empty(), "empty extrinsic");
		if hex::encode(sp_crypto_hashing::blake2_256(&bytes)) != self.hash.trim_start_matches("0x")
		{
			bail!("transaction hash mismatch");
		}
		Ok(bytes)
	}
}
