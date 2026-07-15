//! The portable, human-authored Polkameter test-plan format.
//!
//! XML is deliberately converted into the same `ScenarioDocument` used by the
//! CLI and desktop app. The runner never depends on a file format.

use std::collections::BTreeMap;

use quick_xml::{events::Event, Reader};

use crate::scenario::{
	ArrivalModel, Assertion, ChainTarget, Collector, CompletionBoundary, DevSignerSource,
	DevelopmentFunding, RunLimits, SamplerPhase, ScenarioDocument, TestPlan, ThreadGroup,
	TransactionProfile, TransactionSampler, SCENARIO_VERSION,
};

pub const XML_NAMESPACE: &str = "https://polkameter.dev/schema/plan/v1";

#[derive(Debug, Default)]
struct Node {
	name: String,
	attributes: BTreeMap<String, String>,
	children: Vec<Node>,
	text: String,
}

pub fn parse(xml: &str) -> Result<ScenarioDocument, String> {
	let root = parse_tree(xml)?;
	if root.name != "polkameter-plan" {
		return Err(format!("expected <polkameter-plan>, found <{}>", root.name));
	}
	let version = required_u32(&root, "version")?;
	if version != SCENARIO_VERSION {
		return Err(format!("unsupported XML plan version {version}; expected {SCENARIO_VERSION}"));
	}
	if root.attributes.get("xmlns").map(String::as_str) != Some(XML_NAMESPACE) {
		return Err(format!("<polkameter-plan> must use xmlns=\"{XML_NAMESPACE}\""));
	}
	ensure_attributes(&root, &["xmlns", "version"])?;

	let test_plan = parse_test_plan(required_child(&root, "test-plan")?)?;
	let chain = parse_chain(required_child(&root, "chain")?)?;
	let signer_source = parse_signer(required_child(&root, "signer")?)?;
	let thread_groups = root
		.children
		.iter()
		.filter(|child| child.name == "user-group")
		.map(parse_group)
		.collect::<Result<Vec<_>, _>>()?;
	let collectors = parse_collectors(required_child(&root, "collectors")?)?;
	ensure_children(&root, &["test-plan", "chain", "signer", "user-group", "collectors"])?;

	let document =
		ScenarioDocument { version, test_plan, chain, signer_source, thread_groups, collectors };
	let issues = document.validate();
	if let Some(issue) = issues.first() {
		return Err(format!("invalid XML plan: {} {}", issue.field, issue.message));
	}
	Ok(document)
}

pub fn serialize(document: &ScenarioDocument) -> String {
	let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
	push_line(
		&mut xml,
		0,
		&format!("<polkameter-plan xmlns=\"{XML_NAMESPACE}\" version=\"{}\">", document.version),
	);
	push_line(
		&mut xml,
		1,
		&format!(
			"<test-plan name=\"{}\" seed=\"{}\">",
			escape(&document.test_plan.name),
			document.test_plan.seed
		),
	);
	if !document.test_plan.description.is_empty() {
		push_line(
			&mut xml,
			2,
			&format!("<description>{}</description>", escape(&document.test_plan.description)),
		);
	}
	push_line(&mut xml, 2, &format!(
		"<limits whole-run-timeout-ms=\"{}\" shutdown-drain-timeout-ms=\"{}\" max-concurrent-samples=\"{}\"/>",
		document.test_plan.limits.whole_run_timeout_ms,
		document.test_plan.limits.shutdown_drain_timeout_ms,
		document.test_plan.limits.max_concurrent_samples
	));
	push_line(&mut xml, 1, "</test-plan>");

	let profile = match &document.chain.transaction_profile {
		TransactionProfile::Polkadot => "polkadot".into(),
		TransactionProfile::Custom(value) => format!("custom:{value}"),
	};
	push_line(
		&mut xml,
		1,
		&format!(
			"<chain endpoint=\"{}\" transaction-profile=\"{}\">",
			escape(&document.chain.endpoint),
			escape(&profile)
		),
	);
	if let Some(endpoint) = &document.chain.prometheus_endpoint {
		push_line(&mut xml, 2, &format!("<prometheus endpoint=\"{}\"/>", escape(endpoint)));
	}
	push_line(&mut xml, 1, "</chain>");

	push_line(
		&mut xml,
		1,
		&format!(
			"<signer profile=\"{}\" derivation-path=\"{}\">",
			escape(&document.signer_source.profile),
			escape(&document.signer_source.derivation_path)
		),
	);
	if let Some(funding) = &document.signer_source.funding {
		push_line(
			&mut xml,
			2,
			&format!(
				"<funding amount=\"{}\" finality-timeout-ms=\"{}\" batch-size=\"{}\"/>",
				escape(&funding.amount),
				funding.finality_timeout_ms,
				funding.batch_size
			),
		);
	}
	push_line(&mut xml, 1, "</signer>");

	for group in &document.thread_groups {
		push_line(
			&mut xml,
			1,
			&format!(
				"<user-group name=\"{}\" users=\"{}\" concurrency=\"{}\" iterations=\"{}\">",
				escape(&group.name),
				group.users,
				group.concurrency,
				group.iterations
			),
		);
		match group.arrival {
			ArrivalModel::Burst { window_ms } => push_line(
				&mut xml,
				2,
				&format!("<arrival kind=\"burst\" window-ms=\"{window_ms}\"/>"),
			),
			ArrivalModel::Ramp { duration_ms } => push_line(
				&mut xml,
				2,
				&format!("<arrival kind=\"ramp\" duration-ms=\"{duration_ms}\"/>"),
			),
			ArrivalModel::Poisson { rate_per_second } => push_line(
				&mut xml,
				2,
				&format!("<arrival kind=\"poisson\" rate-per-second=\"{rate_per_second}\"/>"),
			),
		}
		for (phase, container) in [
			(SamplerPhase::Setup, "setup"),
			(SamplerPhase::Transaction, "workflow"),
			(SamplerPhase::Teardown, "teardown"),
		] {
			let samplers = group
				.samplers
				.iter()
				.filter(|sampler| same_phase(sampler.phase, phase))
				.collect::<Vec<_>>();
			if samplers.is_empty() {
				continue;
			}
			push_line(&mut xml, 2, &format!("<{container}>"));
			for sampler in samplers {
				serialize_call(&mut xml, sampler, 3);
			}
			push_line(&mut xml, 2, &format!("</{container}>"));
		}
		push_line(&mut xml, 1, "</user-group>");
	}

	push_line(&mut xml, 1, "<collectors>");
	for collector in &document.collectors {
		push_line(&mut xml, 2, &format!("<collector kind=\"{}\"/>", collector_name(collector)));
	}
	push_line(&mut xml, 1, "</collectors>");
	push_line(&mut xml, 0, "</polkameter-plan>");
	xml
}

fn parse_test_plan(node: &Node) -> Result<TestPlan, String> {
	ensure_attributes(node, &["name", "seed"])?;
	ensure_children(node, &["description", "limits"])?;
	let limits = required_child(node, "limits")?;
	ensure_attributes(
		limits,
		&["whole-run-timeout-ms", "shutdown-drain-timeout-ms", "max-concurrent-samples"],
	)?;
	ensure_children(limits, &[])?;
	Ok(TestPlan {
		name: required_attr(node, "name")?.into(),
		description: optional_child(node, "description").map(Node::text).unwrap_or_default(),
		seed: required_u64(node, "seed")?,
		limits: RunLimits {
			whole_run_timeout_ms: required_u64(limits, "whole-run-timeout-ms")?,
			shutdown_drain_timeout_ms: required_u64(limits, "shutdown-drain-timeout-ms")?,
			max_concurrent_samples: required_u32(limits, "max-concurrent-samples")?,
		},
	})
}

fn parse_chain(node: &Node) -> Result<ChainTarget, String> {
	ensure_attributes(node, &["endpoint", "transaction-profile"])?;
	ensure_children(node, &["prometheus"])?;
	let profile = required_attr(node, "transaction-profile")?;
	let transaction_profile = if profile == "polkadot" {
		TransactionProfile::Polkadot
	} else if let Some(value) = profile.strip_prefix("custom:") {
		TransactionProfile::Custom(value.into())
	} else {
		return Err("chain transaction-profile must be polkadot or custom:<name>".into());
	};
	let prometheus_endpoint = optional_child(node, "prometheus")
		.map(|prometheus| -> Result<String, String> {
			ensure_attributes(prometheus, &["endpoint"])?;
			ensure_children(prometheus, &[])?;
			Ok(required_attr(prometheus, "endpoint")?.into())
		})
		.transpose()?;
	Ok(ChainTarget {
		endpoint: required_attr(node, "endpoint")?.into(),
		prometheus_endpoint,
		transaction_profile,
	})
}

fn parse_signer(node: &Node) -> Result<DevSignerSource, String> {
	ensure_attributes(node, &["profile", "derivation-path"])?;
	ensure_children(node, &["funding"])?;
	let funding = optional_child(node, "funding")
		.map(|funding| -> Result<DevelopmentFunding, String> {
			ensure_attributes(funding, &["amount", "finality-timeout-ms", "batch-size"])?;
			ensure_children(funding, &[])?;
			Ok(DevelopmentFunding {
				amount: required_attr(funding, "amount")?.into(),
				finality_timeout_ms: required_u64(funding, "finality-timeout-ms")?,
				batch_size: required_u32(funding, "batch-size")?,
			})
		})
		.transpose()?;
	Ok(DevSignerSource {
		profile: required_attr(node, "profile")?.into(),
		base_suri: String::new(),
		derivation_path: required_attr(node, "derivation-path")?.into(),
		funding,
	})
}

fn parse_group(node: &Node) -> Result<ThreadGroup, String> {
	ensure_attributes(node, &["name", "users", "concurrency", "iterations"])?;
	ensure_children(node, &["arrival", "setup", "workflow", "teardown"])?;
	let arrival = parse_arrival(required_child(node, "arrival")?)?;
	let mut samplers = Vec::new();
	for (container, phase) in [
		("setup", SamplerPhase::Setup),
		("workflow", SamplerPhase::Transaction),
		("teardown", SamplerPhase::Teardown),
	] {
		if let Some(steps) = optional_child(node, container) {
			ensure_attributes(steps, &[])?;
			ensure_children(steps, &["call"])?;
			for call in &steps.children {
				samplers.push(parse_call(call, phase)?);
			}
		}
	}
	Ok(ThreadGroup {
		name: required_attr(node, "name")?.into(),
		users: required_u32(node, "users")?,
		concurrency: required_u32(node, "concurrency")?,
		iterations: required_u32(node, "iterations")?,
		arrival,
		samplers,
	})
}

fn parse_arrival(node: &Node) -> Result<ArrivalModel, String> {
	ensure_children(node, &[])?;
	match required_attr(node, "kind")? {
		"burst" => {
			ensure_attributes(node, &["kind", "window-ms"])?;
			Ok(ArrivalModel::Burst { window_ms: required_u64(node, "window-ms")? })
		},
		"ramp" => {
			ensure_attributes(node, &["kind", "duration-ms"])?;
			Ok(ArrivalModel::Ramp { duration_ms: required_u64(node, "duration-ms")? })
		},
		"poisson" => {
			ensure_attributes(node, &["kind", "rate-per-second"])?;
			Ok(ArrivalModel::Poisson {
				rate_per_second: required_attr(node, "rate-per-second")?
					.parse()
					.map_err(|_| "arrival rate-per-second must be a number")?,
			})
		},
		value => Err(format!("unknown arrival kind {value}")),
	}
}

fn parse_call(node: &Node, phase: SamplerPhase) -> Result<TransactionSampler, String> {
	if node.name != "call" {
		return Err(format!("expected <call>, found <{}>", node.name));
	}
	ensure_attributes(
		node,
		&["label", "pallet", "method", "completion", "mortality-period", "finality-timeout-ms"],
	)?;
	ensure_children(node, &["arguments", "assertion"])?;
	let arguments_text = required_child(node, "arguments")?.text();
	let arguments = serde_json::from_str(&arguments_text)
		.map_err(|error| format!("call arguments must contain JSON: {error} ({arguments_text})"))?;
	let assertions = node
		.children
		.iter()
		.filter(|child| child.name == "assertion")
		.map(parse_assertion)
		.collect::<Result<Vec<_>, _>>()?;
	Ok(TransactionSampler {
		phase,
		label: required_attr(node, "label")?.into(),
		pallet: required_attr(node, "pallet")?.into(),
		call: required_attr(node, "method")?.into(),
		arguments,
		completion: match required_attr(node, "completion")? {
			"submitted" => CompletionBoundary::Submitted,
			"in-block" => CompletionBoundary::InBlock,
			"finalized" => CompletionBoundary::Finalized,
			value => return Err(format!("unknown call completion {value}")),
		},
		mortality_period: required_u32(node, "mortality-period")?,
		finality_timeout_ms: required_u64(node, "finality-timeout-ms")?,
		assertions,
	})
}

fn parse_assertion(node: &Node) -> Result<Assertion, String> {
	ensure_children(node, &[])?;
	match required_attr(node, "kind")? {
		"success" => {
			ensure_attributes(node, &["kind"])?;
			Ok(Assertion::Success)
		},
		"max-elapsed" => {
			ensure_attributes(node, &["kind", "milliseconds"])?;
			Ok(Assertion::MaxElapsed { milliseconds: required_u64(node, "milliseconds")? })
		},
		value => Err(format!("unknown assertion kind {value}")),
	}
}

fn parse_collectors(node: &Node) -> Result<Vec<Collector>, String> {
	ensure_attributes(node, &[])?;
	ensure_children(node, &["collector"])?;
	node.children
		.iter()
		.map(|collector| {
			ensure_attributes(collector, &["kind"])?;
			ensure_children(collector, &[])?;
			match required_attr(collector, "kind")? {
				"jtl" => Ok(Collector::Jtl),
				"events-jsonl" => Ok(Collector::EventsJsonl),
				"telemetry-jsonl" => Ok(Collector::TelemetryJsonl),
				"summary" => Ok(Collector::Summary),
				"svg-plots" => Ok(Collector::SvgPlots),
				value => Err(format!("unknown collector kind {value}")),
			}
		})
		.collect()
}

fn serialize_call(xml: &mut String, sampler: &TransactionSampler, indent: usize) {
	let completion = match sampler.completion {
		CompletionBoundary::Submitted => "submitted",
		CompletionBoundary::InBlock => "in-block",
		CompletionBoundary::Finalized => "finalized",
	};
	push_line(xml, indent, &format!("<call label=\"{}\" pallet=\"{}\" method=\"{}\" completion=\"{completion}\" mortality-period=\"{}\" finality-timeout-ms=\"{}\">", escape(&sampler.label), escape(&sampler.pallet), escape(&sampler.call), sampler.mortality_period, sampler.finality_timeout_ms));
	let arguments =
		serde_json::to_string_pretty(&sampler.arguments).expect("JSON arguments serialize");
	push_line(xml, indent + 1, &format!("<arguments>{}</arguments>", escape(&arguments)));
	for assertion in &sampler.assertions {
		match assertion {
			Assertion::Success => push_line(xml, indent + 1, "<assertion kind=\"success\"/>"),
			Assertion::MaxElapsed { milliseconds } => push_line(
				xml,
				indent + 1,
				&format!("<assertion kind=\"max-elapsed\" milliseconds=\"{milliseconds}\"/>"),
			),
		}
	}
	push_line(xml, indent, "</call>");
}

fn collector_name(collector: &Collector) -> &'static str {
	match collector {
		Collector::Jtl => "jtl",
		Collector::EventsJsonl => "events-jsonl",
		Collector::TelemetryJsonl => "telemetry-jsonl",
		Collector::Summary => "summary",
		Collector::SvgPlots => "svg-plots",
	}
}
fn same_phase(left: SamplerPhase, right: SamplerPhase) -> bool {
	matches!(
		(left, right),
		(SamplerPhase::Setup, SamplerPhase::Setup)
			| (SamplerPhase::Transaction, SamplerPhase::Transaction)
			| (SamplerPhase::Teardown, SamplerPhase::Teardown)
	)
}
fn push_line(xml: &mut String, indent: usize, value: &str) {
	xml.push_str(&"  ".repeat(indent));
	xml.push_str(value);
	xml.push('\n');
}
fn escape(value: &str) -> String {
	value
		.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
		.replace('"', "&quot;")
		.replace('\'', "&apos;")
}

fn parse_tree(xml: &str) -> Result<Node, String> {
	let mut reader = Reader::from_str(xml);
	reader.config_mut().trim_text(false);
	let mut buffer = Vec::new();
	let mut stack = Vec::<Node>::new();
	let mut root = None;
	loop {
		match reader
			.read_event_into(&mut buffer)
			.map_err(|error| format!("invalid XML: {error}"))?
		{
			Event::Start(event) => stack.push(node_from_start(&reader, &event)?),
			Event::Empty(event) => {
				append_node(&mut stack, &mut root, node_from_start(&reader, &event)?)?
			},
			Event::Text(text) => {
				if let Some(node) = stack.last_mut() {
					let decoded = text.decode().map_err(|error| error.to_string())?;
					node.text.push_str(
						&quick_xml::escape::unescape(&decoded)
							.map_err(|error| error.to_string())?,
					);
				}
			},
			Event::CData(text) => {
				if let Some(node) = stack.last_mut() {
					node.text.push_str(&text.decode().map_err(|error| error.to_string())?);
				}
			},
			Event::GeneralRef(reference) => {
				if let Some(node) = stack.last_mut() {
					let reference = reference.decode().map_err(|error| error.to_string())?;
					let value = quick_xml::escape::resolve_predefined_entity(&reference)
						.ok_or_else(|| format!("unsupported XML entity &{reference};"))?;
					node.text.push_str(value);
				}
			},
			Event::End(event) => {
				let node = stack.pop().ok_or("unexpected XML closing tag")?;
				let closing = String::from_utf8_lossy(event.name().as_ref()).into_owned();
				if node.name != closing {
					return Err(format!("expected </{}>, found </{closing}>", node.name));
				}
				append_node(&mut stack, &mut root, node)?;
			},
			Event::Eof => break,
			Event::Decl(_) | Event::Comment(_) | Event::PI(_) | Event::DocType(_) => {},
		}
		buffer.clear();
	}
	if !stack.is_empty() {
		return Err("unclosed XML element".into());
	}
	root.ok_or("XML document has no root element".into())
}

fn node_from_start(
	reader: &Reader<&[u8]>,
	event: &quick_xml::events::BytesStart<'_>,
) -> Result<Node, String> {
	let mut attributes = BTreeMap::new();
	for attribute in event.attributes() {
		let attribute = attribute.map_err(|error| error.to_string())?;
		let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
		let value = attribute
			.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
			.map_err(|error| error.to_string())?
			.into_owned();
		if attributes.insert(key.clone(), value).is_some() {
			return Err(format!("duplicate XML attribute {key}"));
		}
	}
	Ok(Node {
		name: String::from_utf8_lossy(event.name().as_ref()).into_owned(),
		attributes,
		children: Vec::new(),
		text: String::new(),
	})
}

fn append_node(stack: &mut Vec<Node>, root: &mut Option<Node>, node: Node) -> Result<(), String> {
	if let Some(parent) = stack.last_mut() {
		parent.children.push(node);
	} else if root.replace(node).is_some() {
		return Err("XML document has more than one root element".into());
	}
	Ok(())
}

impl Node {
	fn text(&self) -> String {
		self.text.trim().into()
	}
}
fn required_attr<'a>(node: &'a Node, name: &str) -> Result<&'a str, String> {
	node.attributes
		.get(name)
		.map(String::as_str)
		.filter(|value| !value.is_empty())
		.ok_or_else(|| format!("<{}> requires {name}", node.name))
}
fn required_u64(node: &Node, name: &str) -> Result<u64, String> {
	required_attr(node, name)?
		.parse()
		.map_err(|_| format!("<{}> attribute {name} must be a non-negative integer", node.name))
}
fn required_u32(node: &Node, name: &str) -> Result<u32, String> {
	required_u64(node, name)?
		.try_into()
		.map_err(|_| format!("<{}> attribute {name} is too large", node.name))
}
fn required_child<'a>(node: &'a Node, name: &str) -> Result<&'a Node, String> {
	let found = node.children.iter().filter(|child| child.name == name).collect::<Vec<_>>();
	if found.len() == 1 {
		Ok(found[0])
	} else if found.is_empty() {
		Err(format!("<{}> requires <{name}>", node.name))
	} else {
		Err(format!("<{}> may contain only one <{name}>", node.name))
	}
}
fn optional_child<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
	node.children.iter().find(|child| child.name == name)
}
fn ensure_attributes(node: &Node, allowed: &[&str]) -> Result<(), String> {
	if let Some(name) = node.attributes.keys().find(|name| !allowed.contains(&name.as_str())) {
		return Err(format!("<{}> does not allow attribute {name}", node.name));
	}
	Ok(())
}
fn ensure_children(node: &Node, allowed: &[&str]) -> Result<(), String> {
	if let Some(child) = node.children.iter().find(|child| !allowed.contains(&child.name.as_str()))
	{
		return Err(format!("<{}> does not allow <{}>", node.name, child.name));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn document() -> ScenarioDocument {
		ScenarioDocument {
			version: SCENARIO_VERSION,
			test_plan: TestPlan {
				name: "Transfer journey".into(),
				description: "A small user journey".into(),
				seed: 7,
				limits: RunLimits::default(),
			},
			chain: ChainTarget {
				endpoint: "ws://127.0.0.1:9944".into(),
				prometheus_endpoint: None,
				transaction_profile: TransactionProfile::Polkadot,
			},
			signer_source: DevSignerSource {
				profile: "local-dev".into(),
				base_suri: String::new(),
				derivation_path: "//polkameter".into(),
				funding: None,
			},
			thread_groups: vec![ThreadGroup {
				name: "Buyers".into(),
				users: 2,
				concurrency: 1,
				iterations: 1,
				arrival: ArrivalModel::Ramp { duration_ms: 1_000 },
				samplers: vec![
					TransactionSampler {
						phase: SamplerPhase::Setup,
						label: "prepare".into(),
						pallet: "System".into(),
						call: "remark".into(),
						arguments: serde_json::json!({"remark": "hello"}),
						completion: CompletionBoundary::InBlock,
						mortality_period: 4,
						finality_timeout_ms: 1_000,
						assertions: vec![Assertion::Success],
					},
					TransactionSampler {
						phase: SamplerPhase::Transaction,
						label: "transfer".into(),
						pallet: "Balances".into(),
						call: "transfer_keep_alive".into(),
						arguments: serde_json::json!({"value": "1000"}),
						completion: CompletionBoundary::Finalized,
						mortality_period: 4,
						finality_timeout_ms: 1_000,
						assertions: vec![
							Assertion::Success,
							Assertion::MaxElapsed { milliseconds: 500 },
						],
					},
					TransactionSampler {
						phase: SamplerPhase::Teardown,
						label: "finish".into(),
						pallet: "System".into(),
						call: "remark".into(),
						arguments: serde_json::json!({"remark": "done"}),
						completion: CompletionBoundary::Submitted,
						mortality_period: 4,
						finality_timeout_ms: 1_000,
						assertions: vec![],
					},
				],
			}],
			collectors: vec![Collector::Jtl, Collector::Summary],
		}
	}

	#[test]
	fn round_trips_an_ordered_workflow() {
		let original = document();
		let xml = serialize(&original);
		assert!(xml.contains("<workflow>"));
		let parsed = parse(&xml).expect("XML plan parses");
		assert_eq!(parsed.thread_groups[0].samplers.len(), 3);
		assert!(matches!(parsed.thread_groups[0].samplers[0].phase, SamplerPhase::Setup));
		assert_eq!(parsed.thread_groups[0].samplers[1].label, "transfer");
		assert!(matches!(parsed.thread_groups[0].samplers[2].phase, SamplerPhase::Teardown));
		assert_eq!(
			parsed.thread_groups[0].samplers[1].arguments,
			original.thread_groups[0].samplers[1].arguments
		);
	}

	#[test]
	fn rejects_unknown_tags_and_missing_namespace() {
		let xml = serialize(&document())
			.replace("<workflow>", "<branch>")
			.replace("</workflow>", "</branch>");
		assert!(parse(&xml).unwrap_err().contains("does not allow <branch>"));
		let xml = serialize(&document()).replace(&format!(" xmlns=\"{XML_NAMESPACE}\""), "");
		assert!(parse(&xml).unwrap_err().contains("xmlns"));
	}

	#[test]
	fn parses_the_portable_xml_fixture() {
		let fixture = include_str!("../tests/fixtures/valid-scenario.polkameter.xml");
		let document = parse(fixture).expect("fixture parses");
		assert_eq!(document.test_plan.name, "XML transfer journey");
		assert_eq!(document.thread_groups[0].samplers[0].call, "transfer_keep_alive");
	}
}
