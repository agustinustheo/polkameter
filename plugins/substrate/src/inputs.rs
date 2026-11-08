//! Operation inputs, parsed and checked before the plugin touches a chain. Every error names the
//! input and the accepted range. Secret inputs (`seed`, `funder`) never appear in an error.

use anyhow::{Context as _, Result, bail, ensure};
use polkameter_plugin_sdk::chain::{KeyError, Keypair, derive_range, keypair, parse_account};
use serde_json::Value;

/// The derivation prefix of the load signers when a plan sets none.
pub const DEFAULT_DERIVATION: &str = "//polkameter//";
/// Flood signers when a plan sets none.
pub const DEFAULT_FLOOD_SIGNERS: u32 = 16;
/// Probe signers when a plan sets neither `probe-signers` nor `probes`. With `probes`, each
/// probe gets a signer of its own (up to [`MAX_PROBE_SIGNERS`]), so a probe the chain drops
/// never holds back the nonces of later probes.
pub const DEFAULT_PROBE_SIGNERS: u32 = 1;
/// Remark payload bytes when a plan sets none.
pub const DEFAULT_PAYLOAD_BYTES: u32 = 32;
/// The most flood transactions one prepare step builds.
pub const MAX_COUNT: u32 = 1_000_000;
/// The most probe transactions one prepare step builds.
pub const MAX_PROBES: u32 = 100_000;
/// The most flood signers.
pub const MAX_FLOOD_SIGNERS: u32 = 10_000;
/// The most probe signers.
pub const MAX_PROBE_SIGNERS: u32 = 1_000;
/// The largest remark payload.
pub const MAX_PAYLOAD_BYTES: u32 = 16_384;
/// Prepared transactions are written as hex JSON artifacts, which PolkaMeter reads up to 256 MiB
/// each. The flood, probe and state artifacts of one prepare step stay below this in total.
pub const MAX_ARTIFACT_BYTES: u64 = 200 * 1024 * 1024;
/// The longest `expected-seconds`: a week.
pub const MAX_EXPECTED_SECONDS: u64 = 7 * 24 * 3600;

/// A string input.
pub fn text<'a>(inputs: &'a Value, name: &str) -> Result<&'a str> {
	inputs[name].as_str().with_context(|| format!("input {name} is missing"))
}

/// An optional integer input in `range`, or `default` when absent.
pub fn integer(
	inputs: &Value,
	name: &str,
	range: std::ops::RangeInclusive<u64>,
	default: Option<u64>,
) -> Result<u64> {
	let value = match &inputs[name] {
		Value::Null => default.with_context(|| format!("input {name} is missing"))?,
		value => value
			.as_u64()
			.with_context(|| format!("input {name} must be a whole number, got {value}"))?,
	};
	ensure!(
		range.contains(&value),
		"input {name} must be between {} and {}, got {value}",
		range.start(),
		range.end()
	);
	Ok(value)
}

/// A `u32` input in `range`, or `default` when absent.
pub fn small(
	inputs: &Value,
	name: &str,
	range: std::ops::RangeInclusive<u32>,
	default: Option<u32>,
) -> Result<u32> {
	let wide = (*range.start()).into()..=(*range.end()).into();
	Ok(integer(inputs, name, wide, default.map(Into::into))? as u32)
}

/// An amount in the chain's smallest unit: a JSON integer, or a string of digits for amounts
/// above 2^64. `default` applies when the input is absent.
pub fn amount(inputs: &Value, name: &str, default: Option<u128>) -> Result<u128> {
	match &inputs[name] {
		Value::Null => default.with_context(|| format!("input {name} is missing")),
		Value::Number(n) => n.as_u64().map(u128::from).with_context(|| {
			format!("input {name} must be a whole number of the smallest unit, got {n}")
		}),
		Value::String(s) => {
			let digits = s.replace('_', "");
			ensure!(
				!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
				"input {name} must be digits, got {s:?}"
			);
			digits.parse().with_context(|| format!("input {name} does not fit in 128 bits"))
		},
		other => bail!("input {name} must be a number or a string of digits, got {other}"),
	}
}

/// A boolean input, `default` when absent.
pub fn flag(inputs: &Value, name: &str, default: bool) -> Result<bool> {
	match &inputs[name] {
		Value::Null => Ok(default),
		Value::Bool(b) => Ok(*b),
		other => bail!("input {name} must be true or false, got {other}"),
	}
}

/// The endpoint: `ws://` or `wss://`.
pub fn target(inputs: &Value) -> Result<String> {
	let target = text(inputs, "target")?;
	ensure!(
		target.starts_with("ws://") || target.starts_with("wss://"),
		"input target must be a ws:// or wss:// endpoint, got {target:?}"
	);
	Ok(target.to_owned())
}

/// A secret URI input. Errors name the input, never its value.
pub fn secret(inputs: &Value, name: &str) -> Result<Keypair> {
	let uri = text(inputs, name)?;
	keypair(uri).map_err(|error| match error {
		KeyError::Uri(reason) => anyhow::anyhow!(
			"input {name} is not a valid secret URI ({reason}); pass a credential such as //Alice or <mnemonic>"
		),
		other => other.into(),
	})
}

/// `0` (immortal) or a power of two from 4 to 65536.
pub fn mortality(inputs: &Value) -> Result<Option<u64>> {
	let period = integer(inputs, "mortality", 0..=65_536, Some(0))?;
	if period == 0 {
		return Ok(None);
	}
	ensure!(
		period >= 4 && period.is_power_of_two(),
		"input mortality must be 0 (immortal) or a power of two from 4 to 65536, got {period}"
	);
	Ok(Some(period))
}

/// The mortality `init` and the plugin's error messages suggest for a plan that runs about
/// `seconds` after its prepare step on a chain with `block_seconds` blocks: the next power of
/// two of at least twice the blocks the run spans, from 4; `None` when that is above 65536, the
/// longest mortality (sign immortal transactions then).
pub fn mortality_for(seconds: f64, block_seconds: f64) -> Option<u64> {
	let blocks = (2.0 * seconds / block_seconds.max(0.1)).ceil().max(4.0);
	(blocks <= 65_536.0).then(|| (blocks as u64).next_power_of_two())
}

/// Who signs the load: the seed, the derivation prefix and how many flood and probe signers.
/// Flood signers take indices `0..flood`, probe signers `flood..flood + probe`, so the two sets
/// never share an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signers {
	/// The prefix each index is appended to, such as `//polkameter//`.
	pub derivation: String,
	/// Accounts that sign the flood.
	pub flood: u32,
	/// Accounts that sign the probes.
	pub probe: u32,
}

impl Signers {
	/// The signer layout from `derivation`, `flood-signers` and `probe-signers`. Without
	/// `probe-signers`, each of `probes` probes gets its own signer, up to
	/// [`MAX_PROBE_SIGNERS`]; without either, one signer signs every probe.
	pub fn parse(inputs: &Value) -> Result<Self> {
		let probes = match &inputs["probes"] {
			Value::Null => None,
			_ => Some(small(inputs, "probes", 1..=MAX_PROBES, None)?),
		};
		let default_probe = probes.map_or(DEFAULT_PROBE_SIGNERS, |p| p.min(MAX_PROBE_SIGNERS));
		let derivation = match &inputs["derivation"] {
			Value::Null => DEFAULT_DERIVATION.to_owned(),
			Value::String(s) => s.clone(),
			other => bail!("input derivation must be a string, got {other}"),
		};
		ensure!(
			derivation.starts_with('/') && derivation.ends_with('/'),
			"input derivation must start and end with / (such as //polkameter//), got {derivation:?}"
		);
		polkameter_plugin_sdk::chain::junctions(derivation.trim_end_matches('/'))
			.map_err(|e| anyhow::anyhow!("input derivation: {e}"))?;
		Ok(Self {
			derivation,
			flood: small(
				inputs,
				"flood-signers",
				1..=MAX_FLOOD_SIGNERS,
				Some(DEFAULT_FLOOD_SIGNERS),
			)?,
			probe: small(inputs, "probe-signers", 1..=MAX_PROBE_SIGNERS, Some(default_probe))?,
		})
	}

	/// The flood signers, then the probe signers, derived from `seed`.
	pub fn derive(&self, seed: &Keypair) -> Result<(Vec<Keypair>, Vec<Keypair>)> {
		let flood = derive_range(seed, &self.derivation, 0..self.flood)?;
		let probe = derive_range(seed, &self.derivation, self.flood..self.flood + self.probe)?;
		Ok((flood, probe))
	}

	/// The derivation path of signer `index`, for reports. Paths hold no secret.
	pub fn path(&self, index: u32) -> String {
		format!("{}{index}", self.derivation)
	}
}

/// What a prepare step builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
	/// `System.remark` (or `remark_with_event`) with a unique payload of `payload_bytes`.
	Remarks {
		/// Payload size.
		payload_bytes: u32,
		/// Use `remark_with_event`.
		with_event: bool,
	},
	/// `Balances.transfer_keep_alive` of `amount` (the existential deposit when `None`) to `dest`,
	/// or around the ring of the signer's own group when `dest` is `None`.
	Transfers {
		/// Amount per transfer.
		amount: Option<u128>,
		/// One destination for every transfer.
		dest: Option<[u8; 32]>,
	},
}

/// A prepare step's inputs, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepare {
	/// Flood transactions.
	pub count: u32,
	/// Probe transactions.
	pub probes: u32,
	/// Signer layout.
	pub signers: Signers,
	/// Mortality period, `None` for immortal.
	pub mortality: Option<u64>,
	/// How long the plan runs after this step, for checking the mortality.
	pub expected_seconds: Option<u64>,
	/// Tip per transaction.
	pub tip: u128,
	/// The call.
	pub kind: Kind,
}

impl Prepare {
	/// `prepare-remarks` inputs.
	pub fn remarks(inputs: &Value) -> Result<Self> {
		let kind = Kind::Remarks {
			payload_bytes: small(
				inputs,
				"payload-bytes",
				1..=MAX_PAYLOAD_BYTES,
				Some(DEFAULT_PAYLOAD_BYTES),
			)?,
			with_event: flag(inputs, "with-event", false)?,
		};
		Self::common(inputs, kind)
	}

	/// `prepare-transfers` inputs.
	pub fn transfers(inputs: &Value) -> Result<Self> {
		let amount = match &inputs["amount"] {
			Value::Null => None,
			_ => Some(amount(inputs, "amount", None)?),
		};
		ensure!(amount != Some(0), "input amount must be above 0");
		let dest = match &inputs["dest"] {
			Value::Null => None,
			Value::String(s) => {
				Some(parse_account(s).map_err(|e| anyhow::anyhow!("input dest: {e}"))?)
			},
			other => bail!("input dest must be an SS58 address or 0x account, got {other}"),
		};
		Self::common(inputs, Kind::Transfers { amount, dest })
	}

	fn common(inputs: &Value, kind: Kind) -> Result<Self> {
		let count = small(inputs, "count", 1..=MAX_COUNT, None)?;
		let probes = small(inputs, "probes", 1..=MAX_PROBES, None)?;
		let signers = Signers::parse(inputs)?;
		ensure!(
			signers.flood <= count,
			"input flood-signers ({}) must not exceed count ({count})",
			signers.flood
		);
		ensure!(
			signers.probe <= probes,
			"input probe-signers ({}) must not exceed probes ({probes})",
			signers.probe
		);
		let prepared = Self {
			count,
			probes,
			signers,
			mortality: mortality(inputs)?,
			expected_seconds: match &inputs["expected-seconds"] {
				Value::Null => None,
				_ => Some(integer(inputs, "expected-seconds", 1..=MAX_EXPECTED_SECONDS, None)?),
			},
			tip: amount(inputs, "tip", Some(0))?,
			kind,
		};
		let estimate = prepared.artifact_estimate();
		ensure!(
			estimate <= MAX_ARTIFACT_BYTES,
			"count {count} and probes {probes} with these payloads need about {} MiB of prepared transactions and state; keep it under {} MiB by lowering count, probes or payload-bytes",
			estimate.div_ceil(1024 * 1024),
			MAX_ARTIFACT_BYTES / (1024 * 1024)
		);
		Ok(prepared)
	}

	/// A generous estimate of the bytes of the step's three artifacts: the flood and the probes,
	/// each transaction as hex with its hash and metadata, and the state, one entry per
	/// transaction.
	pub fn artifact_estimate(&self) -> u64 {
		let call = match self.kind {
			Kind::Remarks { payload_bytes, .. } => u64::from(payload_bytes) + 8,
			Kind::Transfers { .. } => 56,
		};
		// Signature, address and extensions take about 110 bytes; hex doubles the bytes.
		let per_tx = (call + 120) * 2 + 64 + 200;
		// "<hash>": {"signer": "0x<64 hex>", "nonce": <up to 20 digits>}
		let per_state = 66 + 2 + 2 + 10 + 68 + 10 + 20 + 4;
		(per_tx + per_state) * (u64::from(self.count) + u64::from(self.probes))
	}
}

/// `fund-accounts` inputs, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fund {
	/// The balance every signer should hold at least.
	pub amount: u128,
	/// Signer layout.
	pub signers: Signers,
	/// Transfers per `Utility.batch_all`, when the runtime has it.
	pub batch: u32,
	/// How long to wait for the funding to finalize.
	pub timeout_seconds: u64,
}

impl Fund {
	/// `fund-accounts` inputs.
	pub fn parse(inputs: &Value) -> Result<Self> {
		let amount = amount(inputs, "amount", None)?;
		ensure!(amount > 0, "input amount must be above 0");
		Ok(Self {
			amount,
			signers: Signers::parse(inputs)?,
			batch: small(inputs, "batch-size", 1..=1_000, Some(100))?,
			timeout_seconds: integer(inputs, "timeout-seconds", 1..=3_600, Some(120))?,
		})
	}
}

/// `preflight` calls to check: `Pallet.call` names.
pub fn calls(inputs: &Value) -> Result<Vec<(String, String)>> {
	let names: Vec<String> = match &inputs["calls"] {
		Value::Null => vec!["System.remark".into(), "Balances.transfer_keep_alive".into()],
		Value::Array(items) => items
			.iter()
			.map(|item| {
				item.as_str()
					.map(str::to_owned)
					.with_context(|| format!("input calls must hold strings, got {item}"))
			})
			.collect::<Result<_>>()?,
		other => bail!("input calls must be a list of Pallet.call names, got {other}"),
	};
	names
		.iter()
		.map(|name| {
			let (pallet, call) = name
				.split_once('.')
				.filter(|(p, c)| !p.is_empty() && !c.is_empty() && !c.contains('.'))
				.with_context(|| format!("input calls: {name:?} is not Pallet.call"))?;
			Ok((pallet.to_owned(), call.to_owned()))
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn signer_sets_are_disjoint_and_derivation_is_checked() {
		let signers = Signers::parse(&json!({"flood-signers": 3, "probe-signers": 2})).unwrap();
		let seed = keypair("//Alice").unwrap();
		let (flood, probe) = signers.derive(&seed).unwrap();
		assert_eq!((flood.len(), probe.len()), (3, 2));
		let all: std::collections::HashSet<_> =
			flood.iter().chain(&probe).map(|k| k.public_key().0).collect();
		assert_eq!(all.len(), 5);
		assert_eq!(signers.path(4), "//polkameter//4");
		assert_eq!(
			probe[0].public_key().0,
			keypair("//Alice//polkameter//3").unwrap().public_key().0
		);

		for bad in ["polkameter//", "//polkameter", "//a///b//", ""] {
			assert!(Signers::parse(&json!({"derivation": bad})).is_err(), "{bad}");
		}
		assert!(Signers::parse(&json!({"probe-signers": 0})).is_err());
	}

	#[test]
	fn oversized_floods_are_refused_before_signing() {
		assert!(
			Prepare::remarks(&json!({"count": 1_000_000, "probes": 1, "payload-bytes": 16384}))
				.is_err()
		);
		assert!(Prepare::remarks(&json!({"count": 100_000, "probes": 200})).is_ok());
		// The flood alone fits, but its probes and state push it over.
		let flood = Prepare::remarks(&json!({"count": 250_000, "probes": 1})).unwrap();
		assert!(flood.artifact_estimate() <= MAX_ARTIFACT_BYTES);
		assert!(Prepare::remarks(&json!({"count": 250_000, "probes": 100_000})).is_err());
		let small = Prepare::remarks(&json!({"count": 10, "probes": 5, "flood-signers": 1}))
			.unwrap()
			.artifact_estimate();
		let more = Prepare::remarks(&json!({"count": 10, "probes": 50, "flood-signers": 1}))
			.unwrap()
			.artifact_estimate();
		assert!(more > small);
	}

	#[test]
	fn mortality_covers_twice_the_run() {
		// 306 s of run at 6 s blocks span 51 blocks: twice that, rounded up, is 128.
		assert_eq!(mortality_for(306.0, 6.0), Some(128));
		assert_eq!(mortality_for(1.0, 6.0), Some(4));
		assert_eq!(mortality_for(3600.0, 0.5), Some(16_384));
		assert_eq!(mortality_for(86_400.0, 1.0), None);
		let prepared =
			Prepare::remarks(&json!({"count": 16, "probes": 1, "expected-seconds": 600})).unwrap();
		assert_eq!(prepared.expected_seconds, Some(600));
		assert!(
			Prepare::remarks(&json!({"count": 16, "probes": 1, "expected-seconds": 0})).is_err()
		);
	}
}
