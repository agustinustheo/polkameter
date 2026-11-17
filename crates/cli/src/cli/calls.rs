//! `polkameter calls`: a load test from flags and the user's calls. It writes the plan the user
//! would otherwise write (one `substrate.prepare-calls` step and a constant-rate load), runs it
//! through the same engine as `polkameter run`, and turns the run's summary into one verdict.

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use polkameter_plugin_sdk::observers::summary::{OverallStatus, Summary};
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

/// Share of rate × duration that must be first submitted in the load window.
const DELIVERED: f64 = 0.95;

/// What a transaction's time from first submit to finality is known to be: at least the first,
/// at most the second (`None`: no bound). A finalized tx PolkaMeter timed is exact; one it could
/// not time (its own pauses or reconnects) is at least 0 and at most the time until it saw it
/// final. A tx not in a block is at least as old as it was when the node's pool still held it
/// ready, which is the chain's wait, not PolkaMeter's; otherwise at least 0.
fn latency_bounds(row: &serde_json::Value) -> (u64, Option<u64>) {
	match row["category"].as_str() {
		Some("finalized-ok" | "failed-in-block") => match row["finalizedMs"].as_u64() {
			Some(ms) => (ms, Some(ms)),
			None => (0, row["finalizedMsAtMost"].as_u64()),
		},
		_ => {
			let ready = row["inReadyPool"].as_bool() == Some(true);
			let waited = match (row["poolSnapshotMs"].as_u64(), row["firstSubmitMs"].as_u64()) {
				(Some(snapshot), Some(first)) if ready => snapshot.saturating_sub(first),
				_ => 0,
			};
			(waited, None)
		},
	}
}

/// The p95 of `times` (`None` sorts above every number: no bound).
fn p95_of(mut times: Vec<Option<u64>>) -> Option<u64> {
	if times.is_empty() {
		return None;
	}
	times.sort_unstable_by_key(|t| t.map_or((1, 0), |t| (0, t)));
	let rank = (times.len() as f64 * 0.95).ceil() as usize;
	times[rank.clamp(1, times.len()) - 1]
}

/// A finished run's verdict.
#[derive(Debug, PartialEq, Eq)]
struct Verdict {
	code: i32,
	name: &'static str,
	why: String,
	/// p95 submit to finalized over every tx sent, proven at least this.
	p95_at_least: Option<u64>,
	/// p95 submit to finalized over every tx sent, proven at most this (`None`: no bound).
	p95_at_most: Option<u64>,
}

/// The verdict, from the ledger (`transactions.jsonl`) alone, by one rule in this order:
///
/// 1. FAIL (exit 1) when the chain is proven to miss a threshold on the load it was given: a tx
///    proven lost, or a p95 from first submit to finality proven above `--max-p95`. Proven means
///    more than 5% of the txs sent took longer, counting only times PolkaMeter measured, or a
///    tx the node's pool held ready (a stall that is the chain's).
/// 2. CANNOT TELL (exit 4) when the load was not delivered (fewer than 95% of rate × duration
///    first submitted), or anything is unresolved: a tx not resolved, a tx the node refused, or a
///    p95 not proven within the limit (txs PolkaMeter could not time).
/// 3. PASS (exit 0) otherwise.
///
/// The engine's own verdict is not an input. PolkaMeter's reconnects, pauses, under-delivery and
/// probes, and a chain that builds no block until a tx arrives, can make it FAIL, but none of
/// them is a chain problem on the delivered load.
fn verdict(rows: &[serde_json::Value], target: u64, max_p95_ms: u64) -> Verdict {
	let count = |c: &str| rows.iter().filter(|r| r["category"] == c).count();
	let (lows, highs): (Vec<u64>, Vec<Option<u64>>) = rows.iter().map(latency_bounds).unzip();
	let p95_at_least = p95_of(lows.iter().copied().map(Some).collect());
	let p95_at_most = p95_of(highs);
	let make = |code, name, why: String| Verdict { code, name, why, p95_at_least, p95_at_most };
	let sent = rows.len() as u64;
	if count("lost") > 0 {
		return make(1, "FAIL", format!("{} transactions proven lost", count("lost")));
	}
	if let Some(p95) = p95_at_least.filter(|p| *p > max_p95_ms) {
		let slow = lows.iter().filter(|l| **l > max_p95_ms).count();
		let stalled = rows
			.iter()
			.zip(&lows)
			.filter(|(r, l)| **l > max_p95_ms && r["inReadyPool"].as_bool() == Some(true))
			.count();
		let stall = if stalled > 0 {
			format!(", {stalled} of them still ready in the node's pool (a stall)")
		} else {
			String::new()
		};
		return make(
			1,
			"FAIL",
			format!(
				"p95 submit to finalized at least {} > {}: {slow} of {sent} transactions took longer{stall}",
				show_ms(p95),
				show_ms(max_p95_ms)
			),
		);
	}
	if (sent as f64) < DELIVERED * target as f64 {
		return make(
			CANNOT_TELL,
			"CANNOT TELL",
			format!(
				"the load was not delivered: {sent} of {target} transactions sent ({:.1}%, {:.0}% needed)",
				100.0 * sent as f64 / target.max(1) as f64,
				100.0 * DELIVERED
			),
		);
	}
	if count("unresolved") > 0 {
		return make(
			CANNOT_TELL,
			"CANNOT TELL",
			format!("{} transactions unresolved", count("unresolved")),
		);
	}
	if count("refused") > 0 {
		return make(
			CANNOT_TELL,
			"CANNOT TELL",
			format!("the node refused {} transactions at submit", count("refused")),
		);
	}
	match p95_at_most {
		Some(p95) if p95 <= max_p95_ms => make(
			0,
			"PASS",
			format!(
				"the load was delivered, every transaction resolved, none lost, p95 submit to finalized at most {}",
				show_ms(p95)
			),
		),
		Some(p95) => make(
			CANNOT_TELL,
			"CANNOT TELL",
			format!(
				"p95 submit to finalized is between {} and {}, and the limit is {}: {} transactions PolkaMeter could not time",
				show_ms(p95_at_least.unwrap_or(0)),
				show_ms(p95),
				show_ms(max_p95_ms),
				rows.iter().filter(|r| r["finalizedMs"].is_null()).count()
			),
		),
		None => make(CANNOT_TELL, "CANNOT TELL", "no transaction could be timed".into()),
	}
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
	let ledger = std::fs::read_to_string(dir.join("transactions.jsonl")).unwrap_or_default();
	let rows: Vec<serde_json::Value> =
		ledger.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
	let target = (args.rate * args.duration as f64).floor() as u64;
	let Verdict { code, name, mut why, p95_at_least, p95_at_most } =
		verdict(&rows, target, args.max_p95);
	if let Some(engine) = summary.verdict.map(|v| v.result).filter(|r| *r != OverallStatus::Pass) {
		why = format!("{why} (engine verdict {engine:?}, not used; see summary.md)");
	}
	let loss = &summary.loss;
	let counts = categories(&ledger);
	let report = json!({
		"verdict": name,
		"exitCode": code,
		"reason": why,
		"target": target,
		"sent": loss.sent,
		"finalized": loss.included,
		"failedInBlock": loss.failed_in_block,
		"lost": loss.lost,
		"unresolved": loss.unknown + loss.unverified + loss.in_pool.unwrap_or(0),
		"refused": loss.refused,
		"categories": counts,
		"maxP95Ms": args.max_p95,
		"p95FinalizedMs": loss.p95_finalized_ms,
		"p95AtLeastMs": p95_at_least,
		"p95AtMostMs": p95_at_most,
		"finalizedTimed": loss.finalized_timed,
		"engineVerdict": summary.verdict.map(|v| v.result),
		"stop": summary.stop.rule.name(),
	});
	std::fs::write(dir.join("calls-verdict.json"), format!("{report:#}\n")).ok();
	let count = |c: &str| counts[c].as_u64().unwrap_or(0);
	write_stdout(&format!(
		"{name}: {why}\nsent {} of {target}: finalized-ok {}, failed-in-block {}, lost {}, unresolved {}, refused {}; p95 submit to finalized {} (proven between {} and {}; limit {})\nArtifacts: {}",
		loss.sent,
		count("finalized-ok"),
		count("failed-in-block"),
		count("lost"),
		count("unresolved"),
		count("refused"),
		loss.p95_finalized_ms.map_or("none".into(), show_ms),
		p95_at_least.map_or("none".into(), show_ms),
		p95_at_most.map_or("no bound".into(), show_ms),
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

	/// A ledger row: its category, `finalizedMs`, `finalizedMsAtMost`, and whether the node's pool
	/// held it ready, `waited` ms after its first submit.
	fn row(
		category: &str,
		ms: Option<u64>,
		at_most: Option<u64>,
		waited: Option<u64>,
	) -> serde_json::Value {
		json!({"category": category, "finalizedMs": ms, "finalizedMsAtMost": at_most,
			"firstSubmitMs": 1_000, "poolSnapshotMs": 1_000 + waited.unwrap_or(0),
			"inReadyPool": waited.is_some()})
	}

	fn rows(n: usize, row: &serde_json::Value) -> Vec<serde_json::Value> {
		vec![row.clone(); n]
	}

	/// The pilot's M-mythos b2 run: an outage left 107 of 120 sent, all finalized, 22 untimed
	/// across PolkaMeter's reconnects, p95 47 s under a 60 s limit. The engine said FAIL (a
	/// recovery "stall" on a fork that builds no block without a tx); round 2 passed that on as
	/// FAIL. Nothing proves a chain problem and the load was not delivered: CANNOT TELL.
	#[test]
	fn the_pilot_outage_run_cannot_tell() {
		let mut ledger = rows(85, &row("finalized-ok", Some(47_000), Some(47_000), None));
		ledger.extend(rows(22, &row("finalized-ok", None, Some(58_000), None)));
		let v = verdict(&ledger, 120, 60_000);
		assert_eq!((v.code, v.name), (CANNOT_TELL, "CANNOT TELL"), "{v:?}");
		assert!(v.why.contains("107 of 120"), "{}", v.why);
	}

	#[test]
	fn a_delivered_load_within_the_limit_passes() {
		let mut ledger = rows(94, &row("finalized-ok", Some(5_000), Some(5_000), None));
		ledger.extend(rows(6, &row("failed-in-block", None, Some(9_000), None)));
		let v = verdict(&ledger, 100, 30_000);
		assert_eq!((v.code, v.p95_at_least, v.p95_at_most), (0, Some(5_000), Some(9_000)), "{v:?}");
	}

	/// A chain proven slow on what it was given fails, even when the load fell short.
	#[test]
	fn a_proven_slow_chain_fails_before_under_delivery_is_considered() {
		let mut ledger = rows(70, &row("finalized-ok", Some(5_000), Some(5_000), None));
		ledger.extend(rows(10, &row("finalized-ok", Some(70_000), Some(70_000), None)));
		let v = verdict(&ledger, 100, 60_000);
		assert_eq!(v.code, 1, "{v:?}");
		assert!(v.why.contains("10 of 80"), "{}", v.why);
	}

	/// Times PolkaMeter could not measure (its reconnects or pauses) never prove a FAIL, however
	/// long the upper bound, and they keep a PASS from being proven.
	#[test]
	fn the_tools_own_blind_spots_cannot_tell() {
		let mut ledger = rows(90, &row("finalized-ok", Some(5_000), Some(5_000), None));
		ledger.extend(rows(10, &row("finalized-ok", None, Some(90_000), None)));
		let v = verdict(&ledger, 100, 60_000);
		assert_eq!(
			(v.code, v.p95_at_least, v.p95_at_most),
			(CANNOT_TELL, Some(5_000), Some(90_000))
		);
	}

	/// Txs the node's pool still held ready long after they were sent are the chain's stall; txs
	/// merely not seen in a block are unresolved.
	#[test]
	fn a_stall_fails_only_when_the_chain_held_the_transactions() {
		let mut ledger = rows(90, &row("finalized-ok", Some(5_000), Some(5_000), None));
		ledger.extend(rows(10, &row("unresolved", None, None, Some(400_000))));
		let v = verdict(&ledger, 100, 60_000);
		assert_eq!(v.code, 1, "{v:?}");
		assert!(v.why.contains("still ready in the node's pool"), "{}", v.why);
		ledger.truncate(90);
		ledger.extend(rows(10, &row("unresolved", None, None, None)));
		assert_eq!(verdict(&ledger, 100, 60_000).code, CANNOT_TELL);
	}

	#[test]
	fn a_loss_fails_and_a_refusal_cannot_tell() {
		let ok = row("finalized-ok", Some(5_000), Some(5_000), None);
		let mut ledger = rows(99, &ok);
		ledger.push(row("lost", None, None, None));
		assert_eq!(verdict(&ledger, 100, 60_000).code, 1);
		ledger.pop();
		ledger.push(row("refused", None, None, None));
		assert_eq!(verdict(&ledger, 100, 60_000).code, CANNOT_TELL);
		assert_eq!(verdict(&[], 100, 60_000).code, CANNOT_TELL);
	}
}
