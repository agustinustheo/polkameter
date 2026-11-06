//! Operation inputs, parsed and checked before the plugin touches a chain. Every error names the
//! input and the accepted range. Secret inputs (`seed`, `funder`) never appear in an error.

use anyhow::{Context as _, Result, bail, ensure};
use polkameter_plugin_sdk::chain::{KeyError, Keypair, derive_range, keypair, parse_account};
use serde_json::Value;

/// The derivation prefix of the load signers when a plan sets none.
pub const DEFAULT_DERIVATION: &str = "//polkameter//";
/// Flood signers when a plan sets none.
pub const DEFAULT_FLOOD_SIGNERS: u32 = 16;
/// Probe signers when a plan sets none.
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
/// Prepared transactions are written as hex JSON artifacts, which PolkaMeter reads up to 256 MiB.
/// Stay well below it.
pub const MAX_ARTIFACT_BYTES: u64 = 200 * 1024 * 1024;

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
	/// The signer layout from `derivation`, `flood-signers` and `probe-signers`.
	pub fn parse(inputs: &Value) -> Result<Self> {
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
			probe: small(
				inputs,
				"probe-signers",
				1..=MAX_PROBE_SIGNERS,
				Some(DEFAULT_PROBE_SIGNERS),
			)?,
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
			tip: amount(inputs, "tip", Some(0))?,
			kind,
		};
		let estimate = prepared.artifact_estimate();
		ensure!(
			estimate <= MAX_ARTIFACT_BYTES,
			"count {count} with these payloads needs about {} MiB of prepared transactions; keep it under {} MiB by lowering count or payload-bytes",
			estimate / (1024 * 1024),
			MAX_ARTIFACT_BYTES / (1024 * 1024)
		);
		Ok(prepared)
	}

	/// A generous estimate of the flood artifact's size: each transaction as hex, with its hash
	/// and metadata.
	pub fn artifact_estimate(&self) -> u64 {
		let call = match self.kind {
			Kind::Remarks { payload_bytes, .. } => u64::from(payload_bytes) + 8,
			Kind::Transfers { .. } => 56,
		};
		// Signature, address and extensions take about 110 bytes; hex doubles the bytes.
		let per_tx = (call + 120) * 2 + 64 + 200;
		per_tx * u64::from(self.count)
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

	fn error(result: Result<impl std::fmt::Debug>) -> String {
		result.unwrap_err().to_string()
	}

	#[test]
	fn remarks_take_defaults_and_check_ranges() {
		let prepared = Prepare::remarks(&json!({"count": 100, "probes": 20})).unwrap();
		assert_eq!(prepared.count, 100);
		assert_eq!(prepared.probes, 20);
		assert_eq!(
			prepared.signers,
			Signers {
				derivation: DEFAULT_DERIVATION.into(),
				flood: DEFAULT_FLOOD_SIGNERS,
				probe: DEFAULT_PROBE_SIGNERS
			}
		);
		assert_eq!(prepared.mortality, None);
		assert_eq!(prepared.tip, 0);
		assert_eq!(prepared.kind, Kind::Remarks { payload_bytes: 32, with_event: false });

		assert_eq!(error(Prepare::remarks(&json!({"probes": 1}))), "input count is missing");
		assert_eq!(
			error(Prepare::remarks(&json!({"count": 0, "probes": 1}))),
			"input count must be between 1 and 1000000, got 0"
		);
		assert_eq!(
			error(Prepare::remarks(&json!({"count": "ten", "probes": 1}))),
			"input count must be a whole number, got \"ten\""
		);
		assert_eq!(
			error(Prepare::remarks(&json!({"count": 4, "probes": 1}))),
			"input flood-signers (16) must not exceed count (4)"
		);
		assert_eq!(
			error(Prepare::remarks(&json!({"count": 100, "probes": 1, "payload-bytes": 0}))),
			"input payload-bytes must be between 1 and 16384, got 0"
		);
		assert_eq!(
			error(Prepare::remarks(&json!({"count": 100, "probes": 1, "with-event": "yes"}))),
			"input with-event must be true or false, got \"yes\""
		);
	}

	#[test]
	fn mortality_is_immortal_or_a_power_of_two() {
		let base = |m: Value| Prepare::remarks(&json!({"count": 16, "probes": 1, "mortality": m}));
		assert_eq!(base(json!(0)).unwrap().mortality, None);
		assert_eq!(base(json!(64)).unwrap().mortality, Some(64));
		assert_eq!(base(json!(65536)).unwrap().mortality, Some(65536));
		assert!(error(base(json!(2))).contains("power of two from 4 to 65536"));
		assert!(error(base(json!(100))).contains("power of two"));
		assert!(error(base(json!(131072))).contains("between 0 and 65536"));
	}

	#[test]
	fn amounts_accept_integers_and_digit_strings() {
		let parse = |v: Value| amount(&json!({"amount": v}), "amount", None);
		assert_eq!(parse(json!(5)).unwrap(), 5);
		assert_eq!(parse(json!("1_000_000_000_000_000_000_000")).unwrap(), 10u128.pow(21));
		assert_eq!(parse(json!(u64::MAX)).unwrap(), u128::from(u64::MAX));
		assert!(error(parse(json!(-1))).contains("whole number"));
		assert!(error(parse(json!(1.5))).contains("whole number"));
		assert!(error(parse(json!("12a"))).contains("must be digits"));
		assert!(error(parse(json!("1".repeat(40)))).contains("128 bits"));
		assert!(error(parse(json!(true))).contains("number or a string of digits"));
		assert_eq!(error(parse(Value::Null)), "input amount is missing");
		assert_eq!(amount(&json!({}), "tip", Some(0)).unwrap(), 0);
	}

	#[test]
	fn transfers_check_amount_and_destination() {
		let alice = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
		let ok = Prepare::transfers(
			&json!({"count": 32, "probes": 2, "amount": "500", "dest": alice, "flood-signers": 2}),
		)
		.unwrap();
		match ok.kind {
			Kind::Transfers { amount: Some(500), dest: Some(dest) } => {
				assert_eq!(dest, parse_account(alice).unwrap())
			},
			other => panic!("{other:?}"),
		}
		let default = Prepare::transfers(&json!({"count": 32, "probes": 2})).unwrap();
		assert_eq!(default.kind, Kind::Transfers { amount: None, dest: None });
		assert_eq!(
			error(Prepare::transfers(&json!({"count": 32, "probes": 2, "amount": 0}))),
			"input amount must be above 0"
		);
		assert!(
			error(Prepare::transfers(&json!({"count": 32, "probes": 2, "dest": "nobody"})))
				.starts_with("input dest: account \"nobody\" is not valid")
		);
	}

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
		assert!(
			error(Signers::parse(&json!({"probe-signers": 0})))
				.contains("probe-signers must be between 1 and 1000")
		);
	}

	#[test]
	fn secrets_are_never_echoed() {
		let message = error(secret(&json!({"seed": "my secret words//0"}), "seed"));
		assert!(message.starts_with("input seed is not a valid secret URI"), "{message}");
		assert!(!message.contains("secret words"));
		assert_eq!(error(secret(&json!({}), "seed")), "input seed is missing");
		assert!(secret(&json!({"seed": "//Bob"}), "seed").is_ok());
	}

	#[test]
	fn targets_must_be_websockets() {
		assert!(target(&json!({"target": "ws://127.0.0.1:9944"})).is_ok());
		assert!(target(&json!({"target": "wss://rpc.example"})).is_ok());
		assert!(error(target(&json!({"target": "http://x"}))).contains("ws:// or wss://"));
	}

	#[test]
	fn oversized_floods_are_refused_before_signing() {
		let message = error(Prepare::remarks(
			&json!({"count": 1_000_000, "probes": 1, "payload-bytes": 16384}),
		));
		assert!(message.contains("MiB of prepared transactions"), "{message}");
		assert!(Prepare::remarks(&json!({"count": 100_000, "probes": 200})).is_ok());
	}

	#[test]
	fn fund_and_calls_inputs() {
		let fund = Fund::parse(&json!({"amount": 1000})).unwrap();
		assert_eq!((fund.amount, fund.batch, fund.timeout_seconds), (1000, 100, 120));
		assert_eq!(error(Fund::parse(&json!({}))), "input amount is missing");
		assert_eq!(
			calls(&json!({})).unwrap(),
			vec![
				("System".into(), "remark".into()),
				("Balances".into(), "transfer_keep_alive".into())
			]
		);
		assert_eq!(
			calls(&json!({"calls": ["Sudo.sudo"]})).unwrap(),
			vec![("Sudo".into(), "sudo".into())]
		);
		assert!(error(calls(&json!({"calls": ["remark"]}))).contains("is not Pallet.call"));
		assert!(error(calls(&json!({"calls": "System.remark"}))).contains("list of Pallet.call"));
	}
}
