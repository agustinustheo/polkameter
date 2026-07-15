use quick_xml::{events::Event, Reader};
use serde::Serialize;

use crate::scenario::{ArrivalModel, ScenarioDocument};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmxImportReport {
	pub thread_groups: Vec<JmxThreadGroup>,
	pub collectors: Vec<String>,
	pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmxThreadGroup {
	pub name: String,
	pub users: u32,
	pub ramp_seconds: u64,
	pub loops: Option<u32>,
}

pub fn export(document: &ScenarioDocument) -> String {
	let groups = document.thread_groups.iter().map(|group| {
		let ramp_seconds = match group.arrival {
			ArrivalModel::Ramp { duration_ms } => duration_ms.div_ceil(1_000),
			ArrivalModel::Burst { .. } | ArrivalModel::Poisson { .. } => 0,
		};
		format!("<ThreadGroup guiclass=\"ThreadGroupGui\" testclass=\"ThreadGroup\" testname=\"{}\" enabled=\"true\"><stringProp name=\"ThreadGroup.num_threads\">{}</stringProp><stringProp name=\"ThreadGroup.ramp_time\">{ramp_seconds}</stringProp><elementProp name=\"ThreadGroup.main_controller\" elementType=\"LoopController\"><stringProp name=\"LoopController.loops\">{}</stringProp><boolProp name=\"LoopController.continue_forever\">false</boolProp></elementProp></ThreadGroup><hashTree><TestPlan.comments>Polkameter Substrate transaction samplers are retained in the .polkameter.xml scenario.</TestPlan.comments></hashTree>", escape(&group.name), group.users, group.iterations)
	}).collect::<String>();
	format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><jmeterTestPlan version=\"1.2\" properties=\"5.0\"><hashTree><TestPlan guiclass=\"TestPlanGui\" testclass=\"TestPlan\" testname=\"{}\" enabled=\"true\"/><hashTree>{groups}<ResultCollector guiclass=\"SimpleDataWriter\" testclass=\"ResultCollector\" testname=\"Polkameter JTL collector\" enabled=\"true\"/><hashTree/></hashTree></hashTree></jmeterTestPlan>", escape(&document.test_plan.name))
}

pub fn import(xml: &str) -> Result<JmxImportReport, String> {
	let mut reader = Reader::from_str(xml);
	reader.config_mut().trim_text(true);
	let mut buffer = Vec::new();
	let mut groups = Vec::new();
	let mut collectors = Vec::new();
	let mut diagnostics = Vec::new();
	let mut current: Option<JmxThreadGroup> = None;
	let mut property: Option<String> = None;
	loop {
		match reader.read_event_into(&mut buffer).map_err(|error| error.to_string())? {
			Event::Start(event) => {
				let element = String::from_utf8_lossy(event.name().as_ref()).into_owned();
				if element == "ThreadGroup" {
					let name = event
						.attributes()
						.flatten()
						.find(|attribute| attribute.key.as_ref() == b"testname")
						.and_then(|attribute| String::from_utf8(attribute.value.into_owned()).ok())
						.unwrap_or_else(|| "Thread Group".into());
					current = Some(JmxThreadGroup { name, users: 1, ramp_seconds: 0, loops: None });
				} else if element == "ResultCollector" {
					collectors.push("jtl".into());
				} else if matches!(
					element.as_str(),
					"HTTPSampler"
						| "JavaSampler" | "JSR223Sampler"
						| "GenericController"
						| "LoopController"
				) {
					diagnostics.push(format!("{element} is not directly executable by Polkameter; preserve the JMX beside a Substrate scenario."));
				}
				if matches!(element.as_str(), "stringProp" | "intProp") {
					property = event
						.attributes()
						.flatten()
						.find(|attribute| attribute.key.as_ref() == b"name")
						.and_then(|attribute| String::from_utf8(attribute.value.into_owned()).ok());
				}
			},
			Event::Text(text) => {
				if let (Some(group), Some(property)) = (current.as_mut(), property.as_deref()) {
					let decoded = text.decode().map_err(|error| error.to_string())?;
					let value = quick_xml::escape::unescape(&decoded)
						.map_err(|error| error.to_string())?
						.parse::<u64>()
						.ok();
					match property {
						"ThreadGroup.num_threads" => {
							group.users = value.unwrap_or(1).try_into().unwrap_or(u32::MAX)
						},
						"ThreadGroup.ramp_time" => group.ramp_seconds = value.unwrap_or_default(),
						"LoopController.loops" => {
							group.loops = value.and_then(|value| value.try_into().ok())
						},
						_ => {},
					}
				}
			},
			Event::End(event) => {
				let element = event.name();
				if element.as_ref() == b"ThreadGroup" {
					if let Some(group) = current.take() {
						groups.push(group);
					}
				}
				if matches!(element.as_ref(), b"stringProp" | b"intProp") {
					property = None;
				}
			},
			Event::Empty(event) => {
				if event.name().as_ref() == b"ResultCollector" {
					collectors.push("jtl".into());
				}
			},
			Event::Eof => break,
			_ => {},
		}
		buffer.clear();
	}
	if groups.is_empty() {
		diagnostics.push("No JMeter ThreadGroup was found.".into());
	}
	Ok(JmxImportReport { thread_groups: groups, collectors, diagnostics })
}

fn escape(value: &str) -> String {
	value.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn imports_jmeter_thread_group_shape() {
		let report = import(r#"<jmeterTestPlan><ThreadGroup testname="Burst"><stringProp name="ThreadGroup.num_threads">12</stringProp><stringProp name="ThreadGroup.ramp_time">3</stringProp><elementProp><stringProp name="LoopController.loops">2</stringProp></elementProp></ThreadGroup><ResultCollector/></jmeterTestPlan>"#).expect("parsed");
		assert_eq!(report.thread_groups[0].users, 12);
		assert_eq!(report.thread_groups[0].ramp_seconds, 3);
		assert_eq!(report.thread_groups[0].loops, Some(2));
		assert_eq!(report.collectors, vec!["jtl"]);
	}

	#[test]
	fn exported_plan_has_importable_thread_groups() {
		let xml = export(&crate::artifacts::test_scenario());
		assert!(xml.contains(".polkameter.xml scenario"));
		assert!(!xml.contains(".polkameter.json scenario"));
		let report = import(&xml).expect("export is importable");
		assert_eq!(report.thread_groups.len(), 1);
		assert_eq!(report.thread_groups[0].users, 1);
		assert_eq!(report.thread_groups[0].loops, Some(1));
		assert_eq!(report.collectors, vec!["jtl"]);
	}
}
