//! `polkameter init`: a starter load-test plan for the substrate plugin, written from what a live
//! chain reports: its calls, its block time and its token.

use anyhow::{Result, ensure};
use polkameter_plugin_sdk::chain::Client;
use serde_json::Value;

/// The call the generated plan floods the chain with.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum Kind {
	/// `System.remark` with a unique 64-byte payload.
	Remarks,
	/// `Balances.transfer_keep_alive` of the existential deposit between load signers.
	Transfers,
}

impl Kind {
	fn call(self) -> (&'static str, &'static str) {
		match self {
			Self::Remarks => ("System", "remark"),
			Self::Transfers => ("Balances", "transfer_keep_alive"),
		}
	}
}

/// What `init` learned from the chain.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
	/// `system_chain`.
	pub chain: String,
	/// The runtime's spec name and version.
	pub runtime: String,
	/// Seconds between blocks, measured from `Timestamp.Now`.
	pub block_seconds: f64,
	/// The token's decimals, from `system_properties` (12 when the chain reports none).
	pub decimals: u32,
	/// The token's symbol, if the chain reports one.
	pub symbol: Option<String>,
}

/// Connects to `rpc`, checks that it has the call `kind` needs and reads the facts the plan uses.
pub async fn inspect(rpc: &str, kind: Kind) -> Result<Facts> {
	let client = Client::connect(rpc)
		.await
		.map_err(|e| anyhow::anyhow!("cannot connect to {rpc}: {e}"))?;
	let builder = client.tx_builder().await?;
	let (pallet, call) = kind.call();
	ensure!(
		builder.has_call(pallet, call),
		"the runtime at {rpc} has no {pallet}.{call}; the substrate plugin cannot load it. Write a plugin for your runtime instead (see Writing a plugin)"
	);
	ensure!(
		builder.has_call("Balances", "transfer_keep_alive"),
		"the runtime at {rpc} has no Balances.transfer_keep_alive, which the substrate plugin uses to fund its signers"
	);
	let chain: String = client.request("system_chain", subxt::rpcs::rpc_params![]).await?;
	let version: Value =
		client.request("state_getRuntimeVersion", subxt::rpcs::rpc_params![]).await?;
	let properties: Value = client
		.request("system_properties", subxt::rpcs::rpc_params![])
		.await
		.unwrap_or_default();
	let first = |v: &Value| if v.is_array() { v[0].clone() } else { v.clone() };
	Ok(Facts {
		chain,
		runtime: format!(
			"{} {}",
			version["specName"].as_str().unwrap_or("unknown"),
			version["specVersion"]
		),
		block_seconds: block_seconds(&client).await?,
		decimals: first(&properties["tokenDecimals"])
			.as_u64()
			.and_then(|d| u32::try_from(d).ok())
			.filter(|d| *d <= 30)
			.unwrap_or(12),
		symbol: first(&properties["tokenSymbol"]).as_str().map(str::to_owned),
	})
}

/// The mean block time over the last (up to) 10 blocks, from `Timestamp.Now`; 6 s when the chain
/// is too young to tell.
async fn block_seconds(client: &Client) -> Result<f64> {
	let seconds = client.recent_block_seconds().await?.unwrap_or(6.0);
	// One decimal, at least 0.5 s: the plan's block-interval-seconds.
	Ok(if seconds >= 0.5 { (seconds * 10.0).round() / 10.0 } else { 6.0 })
}

/// The generated plan's ramp: 10 to 50 transactions per second, 30 s per step.
const RAMP: [u32; 5] = [10, 20, 30, 40, 50];
const STEP_SECONDS: u32 = 30;
const BASELINE_PROBES: u32 = 3;
const RECOVERY_SECONDS: u32 = 120;

/// How long the generated plan runs after its prepare step, in seconds: the baseline probes
/// (two blocks each), the ramp, the recovery, and ten blocks for the last transactions to be
/// final.
pub fn run_seconds(block_seconds: f64) -> u64 {
	let baseline = f64::from(BASELINE_PROBES) * 2.0 * block_seconds;
	let load = f64::from(STEP_SECONDS) * RAMP.len() as f64;
	(baseline + load + f64::from(RECOVERY_SECONDS) + 10.0 * block_seconds).ceil() as u64
}

/// The mortality, in blocks, for a plan that runs `seconds` after its prepare step: the next
/// power of two of at least twice the blocks it spans, from 4. `0` (immortal) when that is
/// above 65536, the longest mortality.
pub fn mortality(seconds: u64, block_seconds: f64) -> u64 {
	let blocks = (2.0 * seconds as f64 / block_seconds.max(0.1)).ceil().max(4.0);
	if blocks > 65_536.0 { 0 } else { (blocks as u64).next_power_of_two() }
}

/// XML-escapes an attribute value.
fn escape(text: &str) -> String {
	text.replace('&', "&amp;")
		.replace('"', "&quot;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

/// Comment-safe text: XML comments cannot hold `--`.
fn comment(text: &str) -> String {
	text.replace("--", "- -")
}

/// The plan. Its rates ramp from 10 to 50 transactions per second in steps of 30 seconds, which
/// suits a first run on most chains; raise them to find the chain's limit.
pub fn render(rpc: &str, kind: Kind, profile: &str, facts: &Facts) -> String {
	let (pallet, call) = kind.call();
	let operation = match kind {
		Kind::Remarks => "prepare-remarks",
		Kind::Transfers => "prepare-transfers",
	};
	let extra = match kind {
		Kind::Remarks => "\n      <input name=\"payload-bytes\" value=\"64\"/>",
		Kind::Transfers => "",
	};
	let symbol = facts.symbol.as_deref().unwrap_or("tokens");
	// 1000 whole tokens per signer.
	let amount = 1000u128.saturating_mul(10u128.saturating_pow(facts.decimals));
	let version = env!("CARGO_PKG_VERSION");
	let rpc_attr = escape(rpc);
	let profile = escape(profile);
	let header = comment(&format!(
		"Generated by polkameter init from {rpc}: chain {}, runtime {}, {} s blocks.",
		facts.chain, facts.runtime, facts.block_seconds
	));
	let seconds = run_seconds(facts.block_seconds);
	let mortality = mortality(seconds, facts.block_seconds);
	let max_sent: u32 = RAMP.iter().map(|rate| rate * STEP_SECONDS).sum();
	format!(
		r#"<?xml version="1.0" encoding="UTF-8"?>
<!--
  {header}
  A {pallet}.{call} load test with the substrate plugin. Before you run it:
    polkameter plugin install <path-to>/polkameter-substrate-plugin
    polkameter plugin credential {profile} POLKAMETER_SUBSTRATE_SEED
    export POLKAMETER_SUBSTRATE_SEED='<secret URI of a funded account, such as //Alice on a dev chain>'
  The fund step tops up 16 flood signers and one signer per probe, derived from that seed,
  to 1000 {symbol} each.
  The ramp sends at most 30 s x (10 + 20 + 30 + 40 + 50) = {max_sent} transactions.
  The run takes about {seconds} s after the prepare step, so each transaction stays valid for
  {mortality} blocks (0 is immortal): twice that at {block} s blocks. If you lengthen the ramp
  or the recovery, raise expected-seconds; the prepare step then checks the mortality.
  A runtime upgrade during the run invalidates the signed transactions.
-->
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="{name}" mode="stress" timeout-ms="1800000">
  <plugins><plugin id="substrate" version="{version}" protocol="1"/></plugins>
  <targets><target id="chain" endpoint="{rpc_attr}"/></targets>
  <credentials><credential id="seed" profile="{profile}"/></credentials>
  <preflight>
    <step id="chain" use="substrate.preflight">
      <input name="target" ref="targets.chain"/>
      <input name="calls" value="[&quot;{pallet}.{call}&quot;]"/>
    </step>
  </preflight>
  <setup>
    <step id="fund" use="substrate.fund-accounts" timeout-ms="300000">
      <input name="target" ref="targets.chain"/>
      <input name="seed" ref="credentials.seed"/>
      <input name="flood-signers" value="16"/>
      <input name="probes" ref="run.probeBudget"/>
      <input name="amount" value="&quot;{amount}&quot;"/>
    </step>
    <step id="prepare" use="substrate.{operation}" timeout-ms="300000">
      <input name="target" ref="targets.chain"/>
      <input name="seed" ref="credentials.seed"/>
      <input name="flood-signers" value="16"/>
      <input name="count" value="5000"/>
      <input name="probes" ref="run.probeBudget"/>{extra}
      <input name="mortality" value="{mortality}"/>
      <input name="expected-seconds" value="{seconds}"/>
    </step>
  </setup>
  <load target="chain" source-ref="steps.prepare.flood" probes-ref="steps.prepare.probes"
        state-check="substrate.check-state" state-ref="steps.prepare.state"
        baseline-probes="{BASELINE_PROBES}" recovery-seconds="{RECOVERY_SECONDS}" block-interval-seconds="{block}">
    <ramp start="10" step="10" steps="5" seconds="{STEP_SECONDS}"/>
  </load>
</polkameter-plan>
"#,
		name = escape(&format!("{} {pallet}.{call} load", facts.chain)),
		block = facts.block_seconds,
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::engine::plan::Plan;

	fn facts() -> Facts {
		Facts {
			chain: "Development".into(),
			runtime: "parachain-template-runtime 1".into(),
			block_seconds: 6.0,
			decimals: 12,
			symbol: Some("UNIT".into()),
		}
	}

	#[test]
	fn generated_plans_parse_for_odd_endpoints_and_names() {
		let mut odd = facts();
		odd.chain = "A & <B> -- \"C\"".into();
		odd.block_seconds = 2.5;
		for (endpoint, kind, profile, facts) in [
			("ws://127.0.0.1:9944", Kind::Remarks, "substrate-dev", facts()),
			("ws://127.0.0.1:9944", Kind::Transfers, "substrate-dev", facts()),
			("wss://rpc.example/?a=1&b=2", Kind::Remarks, "my&profile", odd),
		] {
			let xml = render(endpoint, kind, profile, &facts);
			let plan = Plan::parse(&xml).unwrap_or_else(|e| panic!("{kind:?}: {e}\n{xml}"));
			assert_eq!(plan.credentials.entries.len(), 1);
		}
	}
}
