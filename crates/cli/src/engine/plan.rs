use crate::engine::xml::{self, Element};
pub use crate::engine::xml::{Location, PlanError};
use crate::load::runner::Mode;
use anyhow::{Result, bail, ensure};
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;
use std::{
	collections::{BTreeMap, BTreeSet},
	fmt::Display,
	path::Path,
};

pub const NAMESPACE: &str = "https://polkameter.dev/schema/plan";

/// The largest plan file PolkaMeter reads, in bytes (2 MiB).
pub const MAX_PLAN_BYTES: usize = 2 * 1024 * 1024;

/// The limit as the error messages state it.
const MAX_PLAN_SIZE: &str = "2 MiB (2097152 bytes)";

/// Reads a plan file as UTF-8 text, reading at most [`MAX_PLAN_BYTES`] + 1 bytes. A regular file
/// or, on Unix, a pipe or FIFO is read: `validate <(envsubst < plan.tmpl)` and
/// `cat plan | polkameter validate /dev/stdin` work. A FIFO that no process writes to reads as
/// empty instead of blocking. Directories, sockets and devices are refused, so a CI job cannot
/// hang on a wrong path. Text that is not UTF-8 is a [`PlanError`] at the first invalid byte.
pub fn read_plan(path: &Path) -> Result<String> {
	use std::io::Read as _;
	let shown = path.display();
	let file = open_plan(path).map_err(|e| anyhow::anyhow!("cannot read plan {shown}: {e}"))?;
	let mut bytes = Vec::new();
	let limit = u64::try_from(MAX_PLAN_BYTES).unwrap_or(u64::MAX) + 1;
	file.take(limit)
		.read_to_end(&mut bytes)
		.map_err(|e| anyhow::anyhow!("cannot read plan {shown}: {e}"))?;
	ensure!(
		bytes.len() <= MAX_PLAN_BYTES,
		"plan {shown} is larger than the {MAX_PLAN_SIZE} limit; split the plan or remove unused \
		 content"
	);
	ensure!(
		!bytes.is_empty(),
		"plan {shown} is empty; if it comes from a pipe or FIFO, check that the command writing \
		 it succeeded"
	);
	// A UTF-16 byte-order mark, or `<` with a zero byte beside it, as UTF-16 writes it.
	let utf16 =
		matches!(bytes[..], [0xFF, 0xFE, ..] | [0xFE, 0xFF, ..] | [b'<', 0, ..] | [0, b'<', ..]);
	ensure!(!utf16, "plan {shown} is UTF-16 text; save it as UTF-8, the encoding PolkaMeter reads");
	String::from_utf8(bytes).map_err(|e| {
		let valid = e.utf8_error().valid_up_to();
		let text = std::str::from_utf8(&e.as_bytes()[..valid]).unwrap_or_default();
		PlanError::at(
			Location::of(text, valid),
			"the plan is not UTF-8 text; save it as UTF-8, the encoding PolkaMeter reads",
		)
	})
}

/// Opens `path` for reading if it is a regular file or, on Unix, a pipe or FIFO (following
/// symbolic links). On Unix the file is opened without blocking, so opening a FIFO never waits
/// for a writer, and then checked, so it cannot be swapped between the check and the open. Reads
/// then block again, up to the end of the data.
fn open_plan(path: &Path) -> std::io::Result<std::fs::File> {
	let mut options = std::fs::OpenOptions::new();
	options.read(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.custom_flags(nix::libc::O_NONBLOCK);
	}
	#[cfg(not(unix))]
	refuse_irregular(&std::fs::metadata(path)?)?;
	let file = options.open(path)?;
	refuse_irregular(&file.metadata()?)?;
	#[cfg(unix)]
	{
		use nix::fcntl::{FcntlArg, OFlag, fcntl};
		let flags = OFlag::from_bits_retain(fcntl(&file, FcntlArg::F_GETFL)?);
		fcntl(&file, FcntlArg::F_SETFL(flags - OFlag::O_NONBLOCK))?;
	}
	Ok(file)
}

fn refuse_irregular(metadata: &std::fs::Metadata) -> std::io::Result<()> {
	let kind = metadata.file_type();
	#[cfg(unix)]
	let pipe = std::os::unix::fs::FileTypeExt::is_fifo(&kind);
	#[cfg(not(unix))]
	let pipe = false;
	if kind.is_file() || pipe {
		return Ok(());
	}
	Err(std::io::Error::other(if kind.is_dir() {
		"it is a directory; pass a plan file such as plan.polkameter.xml"
	} else {
		"it is a socket or device, not a plan file; pass a plan file such as plan.polkameter.xml"
	}))
}

/// Reads a plan element from XML; implemented by `element!` and by hand for the special cases.
pub trait FromXml: Sized {
	fn from_xml(el: &Element<'_>) -> Result<Self>;
}

/// One field's reader. `$at` and `$kids` record the names it consumes, for `Element::finish`.
macro_rules! read {
	($el:ident, $at:ident, $kids:ident, attr $a:literal) => {{
		$at.push($a);
		$el.req($a)?
	}};
	($el:ident, $at:ident, $kids:ident, attr $a:literal or $d:expr) => {{
		$at.push($a);
		$el.opt($a)?.unwrap_or($d)
	}};
	($el:ident, $at:ident, $kids:ident, opt_attr $a:literal) => {{
		$at.push($a);
		$el.opt($a)?
	}};
	($el:ident, $at:ident, $kids:ident, list $c:literal) => {{
		$kids.push($c);
		$el.all($c)?
	}};
	($el:ident, $at:ident, $kids:ident, opt_child $c:literal) => {{
		$kids.push($c);
		$el.one($c)?
	}};
	($el:ident, $at:ident, $kids:ident, child $c:literal) => {{
		$kids.push($c);
		$el.section($c)?
	}};
	($el:ident, $at:ident, $kids:ident, position $c:literal) => {{ $el.location() }};
}
/// Serializes one field; the element's position is not part of its JSON form.
macro_rules! ser {
	($map:ident, $value:ident, position $a:literal, $f:ident) => {};
	($map:ident, $value:ident, $k:ident $a:literal, $f:ident) => {
		$map.serialize_entry(key!($k $a), &$value.$f)?;
	};
}
/// A field's JSON key: `@name` for an attribute, the element name for a child.
macro_rules! key {
	(attr $a:literal) => {
		concat!("@", $a)
	};
	(opt_attr $a:literal) => {
		concat!("@", $a)
	};
	($kind:ident $c:literal) => {
		$c
	};
}
/// Declares an element once: its struct, its XML reader and its JSON form.
macro_rules! element {
	($(#[$m:meta])* pub struct $name:ident {
		$($(#[$fm:meta])* $f:ident : $t:ty = $k:ident $a:literal $(or $d:expr)?),* $(,)?
	}) => {
		$(#[$m])*
		#[derive(Clone, Debug)]
		pub struct $name {
			$($(#[$fm])* pub $f: $t),*
		}
		impl FromXml for $name {
			fn from_xml(el: &Element<'_>) -> Result<Self> {
				#[allow(unused_mut)]
				let (mut at, mut kids): (Vec<&str>, Vec<&str>) = Default::default();
				let value = Self { $($f: read!(el, at, kids, $k $a $(or $d)?)),* };
				el.finish(&at, &kids)?;
				Ok(value)
			}
		}
		impl Serialize for $name {
			fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
				let mut map = s.serialize_map(None)?;
				$(ser!(map, self, $k $a, $f);)*
				map.end()
			}
		}
	};
}

/// An attribute's type. Numbers ignore surrounding whitespace, as they did before; strings and
/// run modes are taken exactly as written.
trait Attr: Sized {
	fn parse_attr(text: &str) -> std::result::Result<Self, String>;
}
impl Attr for String {
	fn parse_attr(text: &str) -> std::result::Result<Self, String> {
		Ok(text.to_owned())
	}
}
impl Attr for Mode {
	fn parse_attr(text: &str) -> std::result::Result<Self, String> {
		text.parse()
	}
}
macro_rules! numeric_attr {
	($($t:ty: $what:expr),*) => {$(
		impl Attr for $t {
			fn parse_attr(text: &str) -> std::result::Result<Self, String> {
				text.trim_matches([' ', '\t', '\r', '\n'])
					.parse()
					.map_err(|_| format!("must be {}, found `{text}`", $what))
			}
		}
	)*};
}
numeric_attr!(
	u32: format!("a whole number from 0 to {}", u32::MAX),
	u64: format!("a whole number from 0 to {}", u64::MAX),
	usize: format!("a whole number from 0 to {}", usize::MAX),
	f64: "a number, such as 2 or 0.5"
);

/// The candidate closest to `name` by edit distance, if it is close enough to be a likely typo.
pub(crate) fn closest<'a>(
	name: &str,
	candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
	let limit = (name.chars().count().div_ceil(3)).max(1);
	candidates
		.into_iter()
		.map(|candidate| (edit_distance(name, candidate), candidate))
		.filter(|&(distance, _)| distance <= limit)
		.min_by_key(|&(distance, _)| distance)
		.map(|(_, candidate)| candidate)
}

/// The Levenshtein distance between `a` and `b`, in characters.
fn edit_distance(a: &str, b: &str) -> usize {
	let b: Vec<char> = b.chars().collect();
	let mut row: Vec<usize> = (0..=b.len()).collect();
	for (i, ca) in a.chars().enumerate() {
		let mut diagonal = row[0];
		row[0] = i + 1;
		for (j, cb) in b.iter().enumerate() {
			let above = row[j + 1];
			row[j + 1] = (above + 1).min(row[j] + 1).min(diagonal + usize::from(ca != *cb));
			diagonal = above;
		}
	}
	row[b.len()]
}

/// `names` in backticks, joined with commas, for an error message.
fn backticked<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
	names.into_iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
}

impl Element<'_> {
	fn err(&self, msg: impl Display) -> anyhow::Error {
		self.error_at(self.offset, msg)
	}
	fn opt<T: Attr>(&self, name: &str) -> Result<Option<T>> {
		let Some((_, value, at)) = self.attrs.iter().find(|a| a.0 == name) else {
			return Ok(None);
		};
		let parsed = T::parse_attr(value)
			.map_err(|e| self.error_at(*at, format!("`{name}` on <{}> {e}", self.name)))?;
		Ok(Some(parsed))
	}
	fn req<T: Attr>(&self, name: &str) -> Result<T> {
		self.opt(name)?
			.ok_or_else(|| self.err(format!("missing `{name}` on <{}>", self.name)))
	}
	fn all<T: FromXml>(&self, name: &str) -> Result<Vec<T>> {
		self.children.iter().filter(|c| c.name == name).map(T::from_xml).collect()
	}
	fn one<T: FromXml>(&self, name: &str) -> Result<Option<T>> {
		let found: Vec<_> = self.children.iter().filter(|c| c.name == name).collect();
		match found[..] {
			[] => Ok(None),
			[child] => T::from_xml(child).map(Some),
			[_, extra, ..] => {
				let msg = format!("<{name}> may appear once in <{}>", self.name);
				Err(self.error_at(extra.offset, msg))
			},
		}
	}
	fn section<T: FromXml + Default>(&self, name: &str) -> Result<T> {
		Ok(self.one(name)?.unwrap_or_default())
	}
	/// Rejects attributes and children that no reader consumed; `attrs` and `kids` list them.
	fn finish(&self, attrs: &[&str], kids: &[&str]) -> Result<()> {
		if let Some((key, _, at)) = self.attrs.iter().find(|a| !attrs.contains(&a.0)) {
			let name = self.name;
			let mut msg = format!("unknown attribute `{key}` on <{name}>");
			match closest(key, attrs.iter().copied()) {
				Some(near) => msg += &format!("; did you mean `{near}`? "),
				None => msg += "; ",
			}
			if attrs.is_empty() {
				msg += &format!("<{name}> takes no attributes");
			} else {
				msg += &format!("<{name}> allows {}", backticked(attrs.iter().copied()));
			}
			return Err(self.error_at(*at, msg));
		}
		if let Some(child) = self.children.iter().find(|c| !kids.contains(&c.name)) {
			let mut msg = format!("unknown element <{}> in <{}>", child.name, self.name);
			match closest(child.name, kids.iter().copied()) {
				Some(near) => msg += &format!("; did you mean <{near}>? "),
				None => msg += "; ",
			}
			if kids.is_empty() {
				msg += &format!("<{}> has no child elements", self.name);
			} else {
				let allowed: Vec<_> = kids.iter().map(|kid| format!("<{kid}>")).collect();
				msg += &format!("<{}> may contain {}", self.name, allowed.join(", "));
			}
			return Err(self.error_at(child.offset, msg));
		}
		Ok(())
	}
}

element! {
	pub struct Plan {
		/// Where the element starts in the plan file.
		location: Location = position "",
		version: u32 = attr "version",
		namespace: String = attr "xmlns",
		name: String = attr "name",
		timeout_ms: u64 = attr "timeout-ms" or 3_600_000,
		mode: Mode = attr "mode" or Mode::Stress,
		plugins: Plugins = child "plugins",
		targets: Targets = child "targets",
		credentials: Credentials = child "credentials",
		preflight: Steps = child "preflight",
		setup: Steps = child "setup",
		load: Option<Load> = opt_child "load",
		monitors: Option<Monitors> = opt_child "monitors",
		thresholds: Thresholds = child "thresholds",
		evaluate: Steps = child "evaluate",
		teardown: Steps = child "teardown",
	}
}
element! {
	#[derive(Default)]
	pub struct Plugins {
		entries: Vec<PluginSpec> = list "plugin",
	}
}
element! {
	pub struct PluginSpec {
		location: Location = position "",
		id: String = attr "id",
		version: String = attr "version",
		protocol: u32 = attr "protocol",
	}
}
element! {
	#[derive(Default)]
	pub struct Targets {
		entries: Vec<Target> = list "target",
	}
}
element! {
	pub struct Target {
		location: Location = position "",
		id: String = attr "id",
		endpoint: String = attr "endpoint",
	}
}
element! {
	#[derive(Default)]
	pub struct Credentials {
		entries: Vec<Credential> = list "credential",
	}
}
element! {
	pub struct Credential {
		location: Location = position "",
		id: String = attr "id",
		profile: String = attr "profile",
	}
}
element! {
	#[derive(Default)]
	pub struct Steps {
		entries: Vec<Step> = list "step",
	}
}
element! {
	pub struct Step {
		location: Location = position "",
		id: String = attr "id",
		/// `plugin.operation`; `Plan::validate` checks the form and the provider.
		operation: String = attr "use",
		timeout_ms: u64 = attr "timeout-ms" or 60_000,
		inputs: Vec<Input> = list "input",
	}
}
element! {
	pub struct Monitors {
		location: Location = position "",
		metrics: Vec<MetricRequirement> = list "metric",
		/// Alias into the executing host's topology registry.
		topology: String = attr "topology",
		/// The parachain whose collators are scraped and whose relay slots are judged.
		para_id: u32 = attr "para-id",
		relay_target: String = attr "relay-target",
	}
}
element! {
	pub struct MetricRequirement {
		role: String = attr "role",
		name: String = attr "name",
	}
}
element! {
	pub struct Rate {
		rate: f64 = attr "tx-per-second",
		seconds: u32 = attr "seconds",
	}
}
element! {
	#[derive(Default)]
	pub struct Thresholds {
		location: Location = position "",
		stall_ms: Option<u64> = opt_attr "stall-ms",
		finality_stall_ms: Option<u64> = opt_attr "finality-stall-ms",
		max_p95_latency_ms: Option<u64> = opt_attr "max-p95-latency-ms",
		max_submit_reply_ms: Option<u64> = opt_attr "max-submit-reply-ms",
		min_included_ratio: Option<f64> = opt_attr "min-included-ratio",
		max_refused_ratio: Option<f64> = opt_attr "max-refused-ratio",
		pool_refuses_ratio: Option<f64> = opt_attr "pool-refuses-ratio",
		min_send_ratio: Option<f64> = opt_attr "min-send-ratio",
		max_block_gap_factor: Option<f64> = opt_attr "max-block-gap-factor",
		probes_in_a_row: Option<usize> = opt_attr "probes-in-a-row",
		recovered_block_gap_factor: Option<f64> = opt_attr "recovered-block-gap-factor",
		finality_wait_ms: Option<u64> = opt_attr "finality-wait-ms",
	}
}
/// One input of a step: a reference to an earlier output, or a literal. Exactly one is given.
#[derive(Clone, Debug)]
pub struct Input {
	/// Where the element starts in the plan file.
	pub location: Location,
	pub name: String,
	pub source: InputSource,
}
#[derive(Clone, Debug)]
pub enum InputSource {
	/// A reference to an earlier output, such as `steps.seed.value`.
	Ref(String),
	/// A literal; JSON when it parses, otherwise a string.
	Value(String),
}
impl FromXml for Input {
	fn from_xml(el: &Element<'_>) -> Result<Self> {
		let source = match (el.opt("ref")?, el.opt("value")?) {
			(Some(reference), None) => InputSource::Ref(reference),
			(None, Some(value)) => InputSource::Value(value),
			_ => {
				return Err(el.err(
					"<input> needs exactly one of ref or value: ref=\"steps.<id>.<output>\" to \
					 use an earlier value, or value=\"…\" for a literal",
				));
			},
		};
		el.finish(&["name", "ref", "value"], &[])?;
		Ok(Input { location: el.location(), name: el.req("name")?, source })
	}
}
impl Serialize for Input {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let mut map = s.serialize_map(None)?;
		map.serialize_entry("@name", &self.name)?;
		let (reference, value) = match &self.source {
			InputSource::Ref(reference) => (Some(reference), None),
			InputSource::Value(value) => (None, Some(value)),
		};
		map.serialize_entry("@ref", &reference)?;
		map.serialize_entry("@value", &value)?;
		map.end()
	}
}

/// A ramp: `start` transactions per second, grown by `step` or by a factor, for `steps` steps.
#[derive(Clone, Debug)]
pub struct Ramp {
	pub start: f64,
	pub growth: RampGrowth,
	pub steps: u32,
	pub seconds: u32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RampGrowth {
	/// Adds this many transactions per second at each step.
	Step(f64),
	/// Multiplies the rate by this factor at each step.
	Factor(f64),
}
impl FromXml for Ramp {
	fn from_xml(el: &Element<'_>) -> Result<Self> {
		let growth = match (el.opt("step")?, el.opt("growth")?) {
			(Some(step), None) => RampGrowth::Step(step),
			(None, Some(factor)) => RampGrowth::Factor(factor),
			_ => return Err(el.err("ramp needs exactly one of step or growth")),
		};
		el.finish(&["start", "step", "growth", "steps", "seconds"], &[])?;
		Ok(Ramp {
			start: el.req("start")?,
			growth,
			steps: el.req("steps")?,
			seconds: el.req("seconds")?,
		})
	}
}
impl Serialize for Ramp {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let mut map = s.serialize_map(None)?;
		map.serialize_entry("@start", &self.start)?;
		let (step, growth) = match self.growth {
			RampGrowth::Step(step) => (Some(step), None),
			RampGrowth::Factor(growth) => (None, Some(growth)),
		};
		map.serialize_entry("@step", &step)?;
		map.serialize_entry("@growth", &growth)?;
		map.serialize_entry("@steps", &self.steps)?;
		map.serialize_entry("@seconds", &self.seconds)?;
		map.end()
	}
}

/// A state check operation and the reference it is given; both or neither appear in a plan.
#[derive(Clone, Debug)]
pub struct StateCheck {
	pub check: String,
	pub reference: String,
}
#[derive(Clone, Debug)]
pub struct Load {
	/// Where the element starts in the plan file.
	pub location: Location,
	pub target: String,
	pub source: String,
	pub probes_ref: String,
	pub state: Option<StateCheck>,
	pub connections: usize,
	pub baseline_probes: usize,
	pub recovery_seconds: u32,
	pub block_interval_seconds: f64,
	pub rates: Vec<Rate>,
	pub ramp: Option<Ramp>,
}
impl Serialize for Load {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let (check, reference) = match &self.state {
			Some(state) => (Some(&state.check), Some(&state.reference)),
			None => (None, None),
		};
		let mut map = s.serialize_map(None)?;
		map.serialize_entry("@target", &self.target)?;
		map.serialize_entry("@source-ref", &self.source)?;
		map.serialize_entry("@probes-ref", &self.probes_ref)?;
		map.serialize_entry("@state-check", &check)?;
		map.serialize_entry("@state-ref", &reference)?;
		map.serialize_entry("@connections", &self.connections)?;
		map.serialize_entry("@baseline-probes", &self.baseline_probes)?;
		map.serialize_entry("@recovery-seconds", &self.recovery_seconds)?;
		map.serialize_entry("@block-interval-seconds", &self.block_interval_seconds)?;
		map.serialize_entry("rate", &self.rates)?;
		map.serialize_entry("ramp", &self.ramp)?;
		map.end()
	}
}
impl FromXml for Load {
	fn from_xml(el: &Element<'_>) -> Result<Self> {
		let state = match (el.opt("state-check")?, el.opt("state-ref")?) {
			(Some(check), Some(reference)) => Some(StateCheck { check, reference }),
			(None, None) => None,
			_ => return Err(el.err("state-check and state-ref must be given together")),
		};
		el.finish(
			&[
				"target",
				"source-ref",
				"probes-ref",
				"state-check",
				"state-ref",
				"connections",
				"baseline-probes",
				"recovery-seconds",
				"block-interval-seconds",
			],
			&["rate", "ramp"],
		)?;
		Ok(Load {
			location: el.location(),
			target: el.req("target")?,
			source: el.req("source-ref")?,
			probes_ref: el.req("probes-ref")?,
			state,
			connections: el.opt("connections")?.unwrap_or(4),
			baseline_probes: el.opt("baseline-probes")?.unwrap_or(5),
			recovery_seconds: el.opt("recovery-seconds")?.unwrap_or(900),
			block_interval_seconds: el.opt("block-interval-seconds")?.unwrap_or(6.0),
			rates: el.all("rate")?,
			ramp: el.one("ramp")?,
		})
	}
}

impl Input {
	pub fn literal(&self) -> Result<Value> {
		let InputSource::Value(text) = &self.source else { bail!("missing literal") };
		Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone())))
	}
}
impl Load {
	pub fn rate_plan(&self) -> Result<crate::load::Plan> {
		ensure!(
			self.ramp.is_none() || self.rates.is_empty(),
			"<load> has both <rate> and <ramp>; use rates or a ramp, not both"
		);
		let plan = if let Some(r) = &self.ramp {
			ensure!(r.steps > 0, "<ramp> steps must be at least 1");
			ensure!(
				r.steps <= 10_000,
				"<ramp> steps is {}; a ramp has at most 10000 steps",
				r.steps
			);
			ensure!(
				r.start.is_finite() && r.start > 0.0,
				"<ramp> start must be a finite number of transactions per second above 0, found {}",
				r.start
			);
			match r.growth {
				RampGrowth::Factor(growth) => {
					ensure!(
						growth.is_finite() && growth > 0.0,
						"<ramp> growth must be a finite factor above 0, found {growth}"
					);
					crate::load::Plan::geometric(r.start, growth, r.seconds, r.steps)
				},
				RampGrowth::Step(step) => {
					crate::load::Plan::ramp(r.start, step, r.seconds, r.steps)
				},
			}
		} else {
			ensure!(!self.rates.is_empty(), "<load> needs at least one <rate> or a <ramp>");
			crate::load::Plan {
				steps: self
					.rates
					.iter()
					.map(|r| crate::load::plan::StepPlan { seconds: r.seconds, rate: r.rate })
					.collect(),
			}
		};
		plan.check().map_err(|e| {
			anyhow::anyhow!(
				"<load> rate schedule: {e}; each step needs seconds of at least 1 and a finite rate \
				 of 0 or more transactions per second"
			)
		})?;
		ensure!(
			plan.steps.len() <= 10_000,
			"<load> has {} <rate> steps; use at most 10000",
			plan.steps.len()
		);
		Ok(plan)
	}
}

/// What an identifier may contain, for error messages.
const ID_RULE: &str = "use only ASCII letters, digits, `-` and `_`";

/// Fails with `message` at `location`, unless `condition` holds.
macro_rules! check {
	($condition:expr, $location:expr, $($message:tt)+) => {
		if !$condition {
			return Err(PlanError::at($location, format!($($message)+)));
		}
	};
}

/// Plan-wide names the validator tracks while it walks the plan in execution order.
struct Names<'a> {
	/// Declared plugin IDs.
	plugins: BTreeSet<&'a str>,
	/// Every step ID in the plan, to tell a forward reference from an unknown one.
	all_steps: BTreeSet<&'a str>,
	/// Step IDs seen so far, which must be unique across the plan.
	steps: BTreeSet<&'a str>,
}

impl Plan {
	/// Parses and validates plan XML. Errors give the line and column when they apply to one
	/// element; [`PlanError`] carries them separately.
	pub fn parse(xml: &str) -> Result<Self> {
		ensure!(
			xml.len() <= MAX_PLAN_BYTES,
			"the plan is {} bytes, larger than the {MAX_PLAN_SIZE} limit; split the plan or \
			 remove unused content",
			xml.len()
		);
		let root = xml::parse(xml)?;
		if root.name != "polkameter-plan" {
			let msg = format!(
				"the root element is <{}>; a plan's root element must be <polkameter-plan>",
				root.name
			);
			return Err(root.err(msg));
		}
		let plan = Plan::from_xml(&root)?;
		plan.validate()?;
		Ok(plan)
	}
	fn validate(&self) -> Result<()> {
		let root = self.location;
		self.thresholds
			.rules()
			.map_err(|e| PlanError::at(self.thresholds.location, e))?;
		check!(
			self.version == 1,
			root,
			"unsupported plan version {}; this PolkaMeter reads plans with version=\"1\"",
			self.version
		);
		check!(
			self.namespace == NAMESPACE,
			root,
			"xmlns is \"{}\"; a plan's xmlns must be \"{NAMESPACE}\"",
			self.namespace
		);
		check!(!self.name.is_empty(), root, "`name` on <polkameter-plan> must not be empty");
		check!(self.timeout_ms > 0, root, "`timeout-ms` on <polkameter-plan> must be at least 1");
		let mut names = Names {
			plugins: BTreeSet::new(),
			all_steps: self.all_steps().map(|step| step.id.as_str()).collect(),
			steps: BTreeSet::new(),
		};
		for p in &self.plugins.entries {
			let at = p.location;
			check!(identifier(&p.id), at, "plugin ID `{}` is invalid; {ID_RULE}", p.id);
			check!(
				p.id != "core",
				at,
				"plugin ID `core` is reserved for built-in operations; give the plugin another ID"
			);
			check!(
				names.plugins.insert(p.id.as_str()),
				at,
				"plugin ID `{}` is declared twice",
				p.id
			);
			check!(
				p.protocol == 1,
				at,
				"plugin `{}` uses protocol {}; this PolkaMeter supports protocol=\"1\"",
				p.id,
				p.protocol
			);
			check!(
				!p.version.is_empty(),
				at,
				"plugin `{}` needs a `version`: the version its manifest reports (see `polkameter \
				 plugin list`)",
				p.id
			);
		}
		let mut known: BTreeSet<String> =
			["run.id", "run.probeBudget", "run.directory"].map(str::to_owned).into();
		for t in &self.targets.entries {
			let at = t.location;
			check!(identifier(&t.id), at, "target ID `{}` is invalid; {ID_RULE}", t.id);
			check!(
				known.insert(format!("targets.{}", t.id)),
				at,
				"target ID `{}` is declared twice",
				t.id
			);
			check!(
				t.endpoint.starts_with("ws://") || t.endpoint.starts_with("wss://"),
				at,
				"target `{}` has endpoint `{}`; a target must be a WebSocket RPC URL starting with \
				 ws:// or wss://",
				t.id,
				t.endpoint
			);
		}
		for c in &self.credentials.entries {
			let at = c.location;
			check!(identifier(&c.id), at, "credential ID `{}` is invalid; {ID_RULE}", c.id);
			check!(
				known.insert(format!("credentials.{}", c.id)),
				at,
				"credential ID `{}` is declared twice",
				c.id
			);
			check!(
				!c.profile.is_empty(),
				at,
				"credential `{}` needs a `profile`: the name that `polkameter plugin credential` \
				 maps to an environment variable",
				c.id
			);
		}
		self.validate_plan_steps(&self.preflight.entries, "<preflight>", &mut known, &mut names)?;
		self.validate_plan_steps(&self.setup.entries, "<setup>", &mut known, &mut names)?;
		if let Some(load) = &self.load {
			self.validate_load(load, &known, &names)?;
		}
		if let Some(monitors) = &self.monitors {
			let at = monitors.location;
			check!(
				self.load.is_some(),
				at,
				"<monitors> needs a <load> in the same plan; monitors watch a load test"
			);
			check!(
				known.contains(&format!("targets.{}", monitors.relay_target)),
				at,
				"<monitors> relay-target `{}` is not a declared <target>; add it to <targets> or \
				 fix the ID",
				monitors.relay_target
			);
		}
		self.validate_plan_steps(&self.evaluate.entries, "<evaluate>", &mut known, &mut names)?;
		self.validate_plan_steps(&self.teardown.entries, "<teardown>", &mut known, &mut names)?;
		Ok(())
	}
	fn validate_load(&self, load: &Load, known: &BTreeSet<String>, names: &Names) -> Result<()> {
		let at = load.location;
		load.rate_plan().map_err(|e| PlanError::at(at, e))?;
		check!(
			known.contains(&format!("targets.{}", load.target)),
			at,
			"<load> target `{}` is not a declared <target>; add it to <targets> or fix the ID",
			load.target
		);
		for (attribute, reference) in
			[("source-ref", &load.source), ("probes-ref", &load.probes_ref)]
		{
			check!(
				reference_known(known, reference),
				at,
				"<load> {attribute} is `{reference}`, {}",
				undefined(reference, names, "<load>")
			);
		}
		check!(
			(1..=64).contains(&load.connections),
			at,
			"<load> has connections=\"{}\"; use 1 to 64",
			load.connections
		);
		check!(
			(1..=1000).contains(&load.baseline_probes),
			at,
			"<load> has baseline-probes=\"{}\"; use 1 to 1000",
			load.baseline_probes
		);
		check!(
			load.recovery_seconds <= 86_400,
			at,
			"<load> has recovery-seconds=\"{}\"; use at most 86400 (one day)",
			load.recovery_seconds
		);
		check!(
			load.block_interval_seconds.is_finite() && load.block_interval_seconds >= 0.1,
			at,
			"<load> has block-interval-seconds=\"{}\"; use the chain's expected block time, at \
			 least 0.1",
			load.block_interval_seconds
		);
		if let Some(state) = &load.state {
			check!(
				reference_known(known, &state.reference),
				at,
				"<load> state-ref is `{}`, {}",
				state.reference,
				undefined(&state.reference, names, "<load>")
			);
			validate_operation(&state.check, &names.plugins)
				.map_err(|e| PlanError::at(at, format!("<load> state-check: {e}")))?;
		}
		Ok(())
	}
	/// Checks steps in order. References may name `known` (the plan's values and earlier steps);
	/// each step's outputs join `known` for the steps after it.
	fn validate_plan_steps<'a>(
		&self,
		steps: &'a [Step],
		section: &str,
		known: &mut BTreeSet<String>,
		names: &mut Names<'a>,
	) -> Result<()> {
		for step in steps {
			let at = step.location;
			let id = &step.id;
			check!(identifier(id), at, "step ID `{id}` in {section} is invalid; {ID_RULE}");
			check!(
				names.steps.insert(id.as_str()),
				at,
				"step ID `{id}` is declared twice; step IDs must be unique in the whole plan"
			);
			check!(step.timeout_ms > 0, at, "step `{id}`: `timeout-ms` must be at least 1");
			validate_operation(&step.operation, &names.plugins)
				.map_err(|e| PlanError::at(at, format!("step `{id}`: {e}")))?;
			let operation = &step.operation;
			let mut inputs = BTreeSet::new();
			for input in &step.inputs {
				let name = &input.name;
				check!(
					inputs.insert(name),
					input.location,
					"step `{id}` ({operation}): input `{name}` is given twice"
				);
				if let InputSource::Ref(reference) = &input.source {
					check!(
						reference_known(known, reference),
						input.location,
						"step `{id}` ({operation}): input `{name}` refers to `{reference}`, {}",
						undefined(reference, names, "this step")
					);
				}
			}
			known.insert(format!("steps.{id}"));
		}
		Ok(())
	}
	pub fn all_steps(&self) -> impl Iterator<Item = &Step> {
		self.preflight
			.entries
			.iter()
			.chain(&self.setup.entries)
			.chain(&self.evaluate.entries)
			.chain(&self.teardown.entries)
	}
	pub fn values(&self, run_id: &str) -> BTreeMap<String, Value> {
		let mut values: BTreeMap<_, _> = self
			.targets
			.entries
			.iter()
			.map(|t| (format!("targets.{}", t.id), Value::String(t.endpoint.clone())))
			.collect();
		values.insert("run.id".into(), Value::String(run_id.into()));
		// The run's directory; the run sets it once the directory exists.
		values.insert("run.directory".into(), Value::String(String::new()));
		let probes = self.load.as_ref().map_or(0, |load| {
			crate::load::probe_count(
				load.baseline_probes,
				load.recovery_seconds,
				load.block_interval_seconds,
			)
		});
		values.insert("run.probeBudget".into(), Value::from(probes));
		values
	}
}
fn identifier(id: &str) -> bool {
	!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn reference_known(known: &BTreeSet<String>, reference: &str) -> bool {
	known.contains(reference)
		|| reference.rsplit_once('.').is_some_and(|(prefix, _)| known.contains(prefix))
}
/// Why `reference` cannot be used by `user` (`this step` or an element): a step that does not
/// run before it, or a name nothing in the plan defines.
fn undefined(reference: &str, names: &Names, user: &str) -> String {
	let step = reference.strip_prefix("steps.").and_then(|rest| rest.split('.').next());
	match step {
		Some(step) if names.all_steps.contains(step) => {
			format!("but step `{step}` does not run before {user}; move `{step}` earlier")
		},
		_ => "but nothing in the plan defines it; use targets.<id>, credentials.<id>, run.id, \
		      run.directory, run.probeBudget or steps.<earlier step>.<output>"
			.to_owned(),
	}
}
fn validate_operation(operation: &str, plugins: &BTreeSet<&str>) -> Result<()> {
	let Some((plugin, op)) = operation.split_once('.') else {
		bail!(
			"use=\"{operation}\" must name an operation as <plugin>.<operation>, such as \
			 core.echo"
		);
	};
	ensure!(
		identifier(op),
		"use=\"{operation}\" has an invalid operation name `{op}`; after the plugin ID and `.`, \
		 {ID_RULE}"
	);
	ensure!(
		plugin == "core" || plugins.contains(plugin),
		"use=\"{operation}\" names plugin `{plugin}`, which <plugins> does not declare; add \
		 <plugin id=\"{plugin}\" version=\"…\" protocol=\"1\"/> to <plugins>"
	);
	Ok(())
}

fn positive<T: Default + PartialOrd>(value: T) -> bool {
	value > T::default()
}
fn ratio(value: f64) -> bool {
	value.is_finite() && value > 0.0 && value <= 1.0
}
fn factor(value: f64) -> bool {
	value.is_finite() && value > 0.0
}
/// Overrides each rule the plan sets, after checking the value with its predicate; `$limit` says
/// which values the predicate accepts.
macro_rules! overrides {
	($plan:ident, $rules:ident, $($field:ident: $check:ident = $name:literal),* $(,)?) => {
		$(if let Some(value) = $plan.$field {
			let limit = match stringify!($check) {
				"ratio" => "above 0 and at most 1",
				"factor" => "a finite number above 0",
				_ => "at least 1",
			};
			ensure!($check(value), "<thresholds> has {}=\"{}\"; use {}", $name, value, limit);
			$rules.$field = value;
		})*
	};
}
impl Thresholds {
	pub fn rules(&self) -> Result<crate::load::rules::Rules> {
		let mut rules = crate::load::rules::RULES;
		overrides!(self, rules,
			stall_ms: positive = "stall-ms",
			finality_stall_ms: positive = "finality-stall-ms",
			max_p95_latency_ms: positive = "max-p95-latency-ms",
			max_submit_reply_ms: positive = "max-submit-reply-ms",
			min_included_ratio: ratio = "min-included-ratio",
			max_refused_ratio: ratio = "max-refused-ratio",
			pool_refuses_ratio: ratio = "pool-refuses-ratio",
			min_send_ratio: ratio = "min-send-ratio",
			max_block_gap_factor: factor = "max-block-gap-factor",
			probes_in_a_row: positive = "probes-in-a-row",
			recovered_block_gap_factor: factor = "recovered-block-gap-factor",
			finality_wait_ms: positive = "finality-wait-ms",
		);
		Ok(rules)
	}
}
