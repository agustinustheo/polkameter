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
	/// FAIL when the p95 time from submit to finality is above this, such as 30s.
	#[arg(long, value_parser = seconds, default_value = "60s")]
	max_p95: u64,
	/// Amount (smallest unit) each test account is topped up to. Default: a share of what the
	/// seed can spend, at most a million existential deposits.
	#[arg(long)]
	fund: Option<String>,
	/// Mortality in blocks (a power of two up to 4096), 0 for immortal. Default: covers the run.
	#[arg(long)]
	mortality: Option<u64>,
	/// Seconds to watch the chain after the load.
	#[arg(long, default_value_t = 60)]
	recovery: u32,
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
	// Valid from signing through the last send (setup is before signing), with a margin.
	let expected = args.duration + 120;
	let mortality = args.mortality.unwrap_or_else(|| {
		let blocks = (expected as f64 / block_s.max(0.1)).ceil() as u64;
		let period = blocks.max(16).next_power_of_two();
		if period > 4096 { 0 } else { period }
	});
	let mut inputs = vec![
		("target", r#"ref="targets.chain""#.to_owned()),
		("seed", r#"ref="credentials.seed""#.to_owned()),
		("probes", r#"ref="run.probeBudget""#.to_owned()),
		("count", format!(r#"value="{count}""#)),
		("flood-signers", format!(r#"value="{}""#, args.accounts)),
		("mortality", format!(r#"value="{mortality}""#)),
		("expected-seconds", format!(r#"value="{expected}""#)),
	];
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
	let timeout_ms = (args.duration + u64::from(args.recovery) + 3600) * 1000;
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
		recovery = args.recovery,
		rate = args.rate,
		duration = args.duration,
		max_ms = args.max_p95 * 1000,
	))
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
			format!(
				"p95 submit to finalized {:.1} s > {} s",
				p95 as f64 / 1000.0,
				max_p95_ms / 1000
			),
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
	let client = polkameter_plugin_sdk::chain::Client::connect(&args.chain)
		.await
		.map_err(|e| CliError::preflight(format!("cannot connect to {}: {e}", args.chain)))?;
	let block_s = client.recent_block_seconds().await.ok().flatten().unwrap_or(6.0);
	let block_s = (block_s * 10.0).round() / 10.0;
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
	let (code, name, mut why) = verdict(&summary, args.max_p95 * 1000);
	if let Some(engine) = summary.verdict.map(|v| v.result).filter(|r| *r != OverallStatus::Pass) {
		why = format!("{why} (engine verdict {engine:?}; see summary.md)");
	}
	let loss = &summary.loss;
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
		"p95FinalizedMs": loss.p95_finalized_ms,
		"finalizedTimed": loss.finalized_timed,
		"engineVerdict": summary.verdict.map(|v| v.result),
		"stop": summary.stop.rule.name(),
	});
	std::fs::write(dir.join("calls-verdict.json"), format!("{report:#}\n")).ok();
	write_stdout(&format!(
		"{name}: {why}\nsent {}, finalized {} ({} failed in block), refused {}, lost {}, unresolved {}; p95 submit to finalized {}\nArtifacts: {}",
		loss.sent,
		loss.included,
		loss.failed_in_block,
		loss.refused,
		loss.lost.map_or("not counted".into(), |l| l.to_string()),
		report["unresolved"],
		loss.p95_finalized_ms
			.map_or("none".into(), |ms| format!("{:.1} s", ms as f64 / 1000.0)),
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
			max_p95: 30,
			fund: Some("1000".into()),
			mortality: None,
			recovery: 30,
			output: PathBuf::from("runs"),
			plugin: None,
		};
		let plan = Plan::parse(&plan_xml(&args, "0.4.8", 6.0).unwrap()).unwrap();
		crate::engine::execute::validate_core_contracts(&plan).unwrap();
	}
}
