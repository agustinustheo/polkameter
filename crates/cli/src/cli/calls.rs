//! `polkameter calls`: a load test from flags and the user's calls. It writes the plan the user
//! would otherwise write (one `substrate.prepare-calls` step and a constant-rate load), runs it
//! through the same engine as `polkameter run`, and turns the run's summary into one verdict.

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use polkameter_plugin_sdk::observers::summary::{OverallStatus, Rule, Summary};
use serde_json::json;

use super::{CliError, write_stdout};
use crate::engine::{execute::FailOn, plan::Plan, plugins::Registry};
use crate::shutdown::{self, Signals};

/// Exit code when the run cannot tell whether the chain passed.
pub const CANNOT_TELL: i32 = 4;
/// The credential profile of the seed.
const PROFILE: &str = "calls";
/// The variable that holds the seed when `--seed` is not given.
const SEED_VAR: &str = "POLKAMETER_SEED";

/// `polkameter calls` flags.
#[derive(Debug, clap::Args)]
pub struct Args {
	/// The chain's WebSocket endpoint.
	#[arg(long)]
	chain: String,
	/// The seed that funds the test accounts: a secret URI such as //Alice, or for AccountId20
	/// chains //Alith, 0x and a private key, or a mnemonic. Default: $POLKAMETER_SEED.
	#[arg(long)]
	seed: Option<String>,
	/// Test accounts the calls are spread over (round robin).
	#[arg(long, default_value_t = 8)]
	accounts: u32,
	/// Transactions per second.
	#[arg(long)]
	rate: f64,
	/// How long to send, such as 60s or 2m.
	#[arg(long, value_parser = seconds)]
	duration: u64,
	/// The call file: a module exporting `call({ api, i, account, data })`, and optionally
	/// `setup({ api, accounts, submit })`. polkadot-api must resolve from its directory.
	#[arg(long, conflicts_with = "call")]
	calls: Option<PathBuf>,
	/// One call instead of a call file, as Pallet.call.
	#[arg(long, required_unless_present = "calls")]
	call: Option<String>,
	/// The call's arguments as JSON: an array in order or an object by name. "$i" is the
	/// transaction's index, "$account" its signer; {"Id": "$account"} is a variant.
	#[arg(long, requires = "call")]
	args: Option<String>,
	/// FAIL when the p95 time from submit to finality is above this, such as 30s, 29.5s or
	/// 29500ms; PASS needs it at most this.
	#[arg(long, value_parser = millis, default_value = "60s")]
	max_p95: u64,
	/// Amount (smallest unit) each test account is topped up to. Default: a share of what the
	/// seed can spend, at most a million existential deposits.
	#[arg(long)]
	fund: Option<String>,
	/// Mortality in blocks (a power of two up to 4096), 0 for immortal. Default: the longest the
	/// chain keeps, at most 4096 blocks.
	#[arg(long)]
	mortality: Option<u64>,
	/// At most this many seconds to keep watching the chain after the load, until every
	/// transaction is in a block. Default: the longer of 300 s and the duration.
	#[arg(long)]
	recovery: Option<u32>,
	/// The directory that receives the run-* directory.
	#[arg(long, default_value = "polkameter-runs")]
	output: PathBuf,
	/// The substrate plugin executable. Default: next to this polkameter, or the installed one.
	#[arg(long)]
	plugin: Option<PathBuf>,
}

/// `60s`, `2m`, `1m30s` or a number of seconds.
fn seconds(text: &str) -> Result<u64, String> {
	let mut total = 0;
	let mut digits = String::new();
	for c in text.trim().chars() {
		match c {
			'0'..='9' => digits.push(c),
			'h' | 'm' | 's' => {
				let n: u64 = digits.parse().map_err(|_| format!("{text:?} is not a duration"))?;
				total += n * match c {
					'h' => 3600,
					'm' => 60,
					_ => 1,
				};
				digits.clear();
			},
			_ => return Err(format!("{text:?} is not a duration such as 60s or 2m")),
		}
	}
	if !digits.is_empty() {
		total += digits.parse::<u64>().map_err(|_| format!("{text:?} is not a duration"))?;
	}
	if total == 0 {
		return Err(format!("{text:?} must be above 0"));
	}
	Ok(total)
}

/// A time limit in milliseconds: `30s`, `29.5s`, `29500ms`, `1m`, or a number of seconds.
fn millis(text: &str) -> Result<u64, String> {
	let text = text.trim();
	let bad = || format!("{text:?} is not a time such as 30s, 29.5s or 29500ms");
	let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
	let (number, unit) = text.split_at(split);
	let number: f64 = number.parse().map_err(|_| bad())?;
	let scale = match unit {
		"ms" => 1.0,
		"" | "s" => 1000.0,
		"m" => 60_000.0,
		"h" => 3_600_000.0,
		_ => return Err(bad()),
	};
	let ms = (number * scale).round();
	if !(ms.is_finite() && ms >= 1.0) {
		return Err(format!("{text:?} must be at least 1 ms"));
	}
	Ok(ms as u64)
}

/// A time in milliseconds as the shortest text that says it: `30 s`, `29.5 s`, `250 ms`.
fn show_ms(ms: u64) -> String {
	if ms < 1000 { format!("{ms} ms") } else { format!("{} s", ms as f64 / 1000.0) }
}

/// The default bound on recovery: the longer of 300 s and the load's duration.
fn recovery_bound(args: &Args) -> u32 {
	args.recovery
		.unwrap_or_else(|| u32::try_from(args.duration.max(300)).unwrap_or(u32::MAX))
}

/// Escapes text for an XML attribute.
fn attr(text: &str) -> String {
	text.replace('&', "&amp;")
		.replace('"', "&quot;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

/// The substrate plugin: `--plugin`, the one next to this executable, or the installed one.
fn plugin_path(given: Option<PathBuf>) -> Result<PathBuf, CliError> {
	if let Some(path) = given {
		return Ok(path);
	}
	let name = format!("polkameter-substrate-plugin{}", std::env::consts::EXE_SUFFIX);
	if let Some(sibling) =
		std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&name)))
		&& sibling.is_file()
	{
		return Ok(sibling);
	}
	Registry::load()
		.ok()
		.and_then(|r| r.plugins.get("substrate").map(|i| i.executable.clone()))
		.ok_or_else(|| {
			CliError::invalid(format!(
				"cannot find {name}: build it (cargo build --release -p polkameter-substrate-plugin) next to polkameter, or pass --plugin"
			))
		})
}

/// The plan of the run, as XML.
fn plan_xml(args: &Args, version: &str, block_s: f64) -> Result<String, CliError> {
	let count = (args.rate * args.duration as f64 * 1.05).ceil() as u64 + u64::from(args.accounts);
	let recovery = recovery_bound(args);
	// Valid from signing through the last send and recovery (setup is before signing).
	let expected = args.duration + u64::from(recovery) + 120;
	let mut inputs = vec![
		("target", r#"ref="targets.chain""#.to_owned()),
		("seed", r#"ref="credentials.seed""#.to_owned()),
		("probes", r#"ref="run.probeBudget""#.to_owned()),
		("count", format!(r#"value="{count}""#)),
		("flood-signers", format!(r#"value="{}""#, args.accounts)),
		("expected-seconds", format!(r#"value="{expected}""#)),
	];
	// Without --mortality the plugin picks the longest the chain keeps (see its prepare-calls).
	if let Some(mortality) = args.mortality {
		inputs.push(("mortality", format!(r#"value="{mortality}""#)));
	}
	match (&args.calls, &args.call) {
		(Some(file), _) => {
			let file = file
				.canonicalize()
				.map_err(|e| CliError::invalid(format!("--calls {}: {e}", file.display())))?;
			let file = file
				.to_str()
				.ok_or_else(|| CliError::invalid("--calls path is not UTF-8".into()))?;
			inputs.push(("script", format!(r#"value="{}""#, attr(file))));
		},
		(None, Some(call)) => {
			inputs.push(("call", format!(r#"value="{}""#, attr(call))));
			let json = args.args.clone().unwrap_or_else(|| "[]".into());
			serde_json::from_str::<serde_json::Value>(&json)
				.map_err(|e| CliError::invalid(format!("--args is not JSON: {e}")))?;
			// A JSON literal stays a string input: the plugin parses it.
			inputs.push((
				"args",
				format!(r#"value="{}""#, attr(&serde_json::to_string(&json).unwrap_or_default())),
			));
		},
		(None, None) => return Err(CliError::invalid("give --calls or --call".into())),
	}
	if let Some(fund) = &args.fund {
		inputs.push((
			"amount",
			format!(r#"value="{}""#, attr(&serde_json::to_string(fund).unwrap_or_default())),
		));
	}
	let inputs: String = inputs
		.iter()
		.map(|(name, source)| format!("\n      <input name=\"{name}\" {source}/>"))
		.collect();
	let timeout_ms = (args.duration + u64::from(recovery) + 3600) * 1000;
	Ok(format!(
		r#"<?xml version="1.0" encoding="UTF-8"?>
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="calls" mode="stress" timeout-ms="{timeout_ms}">
  <plugins><plugin id="substrate" version="{version}" protocol="1"/></plugins>
  <targets><target id="chain" endpoint="{chain}"/></targets>
  <credentials><credential id="seed" profile="{PROFILE}"/></credentials>
  <setup>
    <step id="prepare" use="substrate.prepare-calls" timeout-ms="3600000">{inputs}
    </step>
  </setup>
  <load target="chain" source-ref="steps.prepare.flood" probes-ref="steps.prepare.probes"
        baseline-probes="2" recovery-seconds="{recovery}" block-interval-seconds="{block_s}">
    <rate tx-per-second="{rate}" seconds="{duration}"/>
  </load>
  <thresholds max-p95-latency-ms="{max_ms}"/>
</polkameter-plan>
"#,
		chain = attr(&args.chain),
		rate = args.rate,
		duration = args.duration,
		max_ms = args.max_p95,
	))
}

/// The categories of `transactions.jsonl` and how many transactions each has.
pub const CATEGORIES: [&str; 5] =
	["finalized-ok", "failed-in-block", "lost", "unresolved", "refused"];

/// How many transactions of the ledger (`transactions.jsonl`) are in each category.
fn categories(ledger: &str) -> serde_json::Value {
	let mut counts: serde_json::Map<String, serde_json::Value> =
		CATEGORIES.iter().map(|c| ((*c).to_owned(), 0.into())).collect();
	for row in ledger.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
		let category = row["category"].as_str().unwrap_or("unresolved");
		let n = counts.get(category).and_then(serde_json::Value::as_u64).unwrap_or(0);
		counts.insert(category.to_owned(), (n + 1).into());
	}
	counts.into()
}

/// The verdict of a finished run: exit code and one line why.
fn verdict(summary: &Summary, max_p95_ms: u64) -> (i32, &'static str, String) {
	let loss = &summary.loss;
	let status = summary.verdict.map(|v| v.result);
	let unresolved = loss.unknown + loss.unverified + loss.in_pool.unwrap_or(0);
	let p95 = loss.p95_finalized_ms;
	if status == Some(OverallStatus::Fail) {
		let why = summary
			.breaking_point
			.as_ref()
			.map(|b| b.detail.clone())
			.or_else(|| summary.failure_modes.first().map(|f| f.what.clone()))
			.unwrap_or_else(|| "a check failed; see summary.md".into());
		return (1, "FAIL", why);
	}
	if loss.lost.is_some_and(|lost| lost > 0) {
		return (1, "FAIL", format!("{} transactions proven lost", loss.lost.unwrap_or(0)));
	}
	if let Some(p95) = p95.filter(|p| *p > max_p95_ms) {
		return (
			1,
			"FAIL",
			format!("p95 submit to finalized {} > {}", show_ms(p95), show_ms(max_p95_ms)),
		);
	}
	// The load was not delivered: it stopped before the rate ran its full time, such as on
	// "generator limit". An engine INCONCLUSIVE for a check outside calls mode's criteria (such
	// as an unmeasured drain) is reported, not a reason to withhold the verdict.
	if summary.stop.rule != Rule::RateCap {
		return (
			CANNOT_TELL,
			"CANNOT TELL",
			format!(
				"the load was not delivered: {} ({})",
				summary.stop.rule.name(),
				summary.stop.detail
			),
		);
	}
	if unresolved > 0 || loss.lost.is_none() {
		return (
			CANNOT_TELL,
			"CANNOT TELL",
			format!("{unresolved} transactions unresolved, or loss not counted"),
		);
	}
	if p95.is_none() {
		return (CANNOT_TELL, "CANNOT TELL", "no finalized transaction could be timed".into());
	}
	(0, "PASS", "every transaction resolved, none lost, p95 within the limit".into())
}

/// The chain's block interval, measured as preflight measures it (on a chain just started, it
/// waits for enough blocks). A connection that drops is opened again for up to 90 s.
async fn block_seconds(chain: &str) -> Result<f64, CliError> {
	let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
	loop {
		let measured = async {
			let client = polkameter_plugin_sdk::chain::Client::connect(chain).await?;
			crate::engine::execute::block_interval_s(&client).await
		}
		.await;
		match measured {
			Ok(seconds) => return Ok((seconds * 100.0).round() / 100.0),
			Err(polkameter_plugin_sdk::chain::ChainError::Timeout(why)) => {
				return Err(CliError::preflight(format!("{chain}: {why}")));
			},
			Err(e) if tokio::time::Instant::now() >= deadline => {
				return Err(CliError::preflight(format!("cannot read blocks of {chain}: {e}")));
			},
			Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
		}
	}
}

/// `polkameter calls`.
pub async fn run(args: Args) -> Result<i32, CliError> {
	if !(args.chain.starts_with("ws://") || args.chain.starts_with("wss://")) {
		return Err(CliError::invalid(format!(
			"--chain must start with ws:// or wss://, got {}",
			args.chain
		)));
	}
	if !(args.rate.is_finite() && args.rate > 0.0) || args.accounts == 0 {
		return Err(CliError::invalid("--rate and --accounts must be above 0".into()));
	}
	let seed = args
		.seed
		.clone()
		.or_else(|| std::env::var(SEED_VAR).ok())
		.filter(|s| !s.is_empty());
	let Some(seed) = seed else {
		return Err(CliError::invalid(format!("pass --seed or set {SEED_VAR}")));
	};
	let mut signals = Signals::install()
		.map_err(|e| CliError::runtime(format!("cannot install signal handlers: {e}")))?;
	let plugin = plugin_path(args.plugin.clone())?;
	let (manifest, installation) = Registry::describe(&plugin)
		.await
		.map_err(|e| CliError::invalid(format!("{e:#}")))?;
	let block_s = block_seconds(&args.chain).await?;
	let xml = plan_xml(&args, &manifest.version, block_s)?;
	let plan = Plan::parse(&xml)
		.map_err(|e| CliError::runtime(format!("the generated plan does not parse: {e:#}")))?;
	let registry = Registry {
		plugins: BTreeMap::from([("substrate".to_owned(), installation)]),
		credentials: BTreeMap::from([(PROFILE.to_owned(), SEED_VAR.to_owned())]),
		inline: BTreeMap::from([(PROFILE.to_owned(), seed)]),
		..Registry::default()
	};
	let grace = Duration::from_secs(shutdown::DEFAULT_GRACE.as_secs());
	let outcome = match super::supervised_run(
		plan,
		registry,
		&args.output,
		FailOn::Error,
		grace,
		&mut signals,
	)
	.await?
	{
		Ok(outcome) => outcome,
		Err(code) => return Ok(code),
	};
	let dir = outcome.artifact_dir.clone();
	std::fs::write(dir.join("plan.polkameter.xml"), &xml).ok();
	let summary: Option<Summary> = std::fs::read(dir.join("summary.json"))
		.ok()
		.and_then(|bytes| serde_json::from_slice(&bytes).ok());
	let Some(summary) = summary else {
		let phase = outcome.failed_phase.clone().unwrap_or_default();
		let code = if matches!(phase.as_str(), "preflight" | "setup") { 3 } else { 1 };
		eprintln!(
			"{}: run failed in {phase}: {} (details in {})",
			crate::PRODUCT_NAME,
			outcome.error.clone().unwrap_or_default(),
			dir.join("execution.json").display()
		);
		return Ok(code);
	};
	let (code, name, mut why) = verdict(&summary, args.max_p95);
	if let Some(engine) = summary.verdict.map(|v| v.result).filter(|r| *r != OverallStatus::Pass) {
		why = format!("{why} (engine verdict {engine:?}; see summary.md)");
	}
	let loss = &summary.loss;
	let counts =
		categories(&std::fs::read_to_string(dir.join("transactions.jsonl")).unwrap_or_default());
	let report = json!({
		"verdict": name,
		"exitCode": code,
		"reason": why,
		"sent": loss.sent,
		"finalized": loss.included,
		"failedInBlock": loss.failed_in_block,
		"lost": loss.lost,
		"unresolved": loss.unknown + loss.unverified + loss.in_pool.unwrap_or(0),
		"refused": loss.refused,
		"categories": counts,
		"maxP95Ms": args.max_p95,
		"p95FinalizedMs": loss.p95_finalized_ms,
		"finalizedTimed": loss.finalized_timed,
		"engineVerdict": summary.verdict.map(|v| v.result),
		"stop": summary.stop.rule.name(),
	});
	std::fs::write(dir.join("calls-verdict.json"), format!("{report:#}\n")).ok();
	let count = |c: &str| counts[c].as_u64().unwrap_or(0);
	write_stdout(&format!(
		"{name}: {why}\nsent {}: finalized-ok {}, failed-in-block {}, lost {}, unresolved {}, refused {}; p95 submit to finalized {} (limit {})\nArtifacts: {}",
		loss.sent,
		count("finalized-ok"),
		count("failed-in-block"),
		count("lost"),
		count("unresolved"),
		count("refused"),
		loss.p95_finalized_ms.map_or("none".into(), show_ms),
		show_ms(args.max_p95),
		dir.display()
	));
	Ok(code)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn durations_and_plans() {
		assert_eq!(seconds("60s"), Ok(60));
		assert_eq!(seconds("1m30s"), Ok(90));
		assert_eq!(seconds("45"), Ok(45));
		assert!(seconds("0s").is_err() && seconds("1x").is_err());
		let args = Args {
			chain: "ws://127.0.0.1:9944".into(),
			seed: None,
			accounts: 4,
			rate: 2.0,
			duration: 30,
			calls: None,
			call: Some("System.remark".into()),
			args: Some(r#"["0x01"]"#.into()),
			max_p95: 30_000,
			fund: Some("1000".into()),
			mortality: None,
			recovery: Some(30),
			output: PathBuf::from("runs"),
			plugin: None,
		};
		let xml = plan_xml(&args, "0.4.8", 6.0).unwrap();
		let plan = Plan::parse(&xml).unwrap();
		crate::engine::execute::validate_core_contracts(&plan).unwrap();
		// Without --mortality the plugin picks it; it is not sized from the block time.
		assert!(!xml.contains(r#"name="mortality""#), "{xml}");
		assert!(xml.contains(r#"max-p95-latency-ms="30000""#), "{xml}");
		let args = Args { mortality: Some(1024), recovery: None, ..args };
		let xml = plan_xml(&args, "0.4.8", 6.0).unwrap();
		assert!(xml.contains(r#"<input name="mortality" value="1024"/>"#), "{xml}");
		assert!(xml.contains(r#"recovery-seconds="300""#), "{xml}");
	}

	#[test]
	fn time_limits_take_milliseconds_and_fractions() {
		assert_eq!(millis("30s"), Ok(30_000));
		assert_eq!(millis("29.5s"), Ok(29_500));
		assert_eq!(millis("29500ms"), Ok(29_500));
		assert_eq!(millis("30"), Ok(30_000));
		assert_eq!(millis("1m"), Ok(60_000));
		assert_eq!(millis("0.25"), Ok(250));
		assert!(millis("0s").is_err() && millis("1x").is_err() && millis("s").is_err());
		assert_eq!(show_ms(29_500), "29.5 s");
		assert_eq!(show_ms(250), "250 ms");
	}

	/// The counts of calls-verdict.json are the categories of transactions.jsonl.
	#[test]
	fn categories_are_counted_from_the_ledger() {
		let ledger =
			["finalized-ok", "finalized-ok", "failed-in-block", "lost", "refused", "unresolved"]
				.map(|c| format!(r#"{{"hash":"00","category":"{c}"}}"#))
				.join("\n");
		let counts = categories(&ledger);
		assert_eq!(
			counts,
			json!({"finalized-ok": 2, "failed-in-block": 1, "lost": 1, "unresolved": 1, "refused": 1})
		);
	}
}
