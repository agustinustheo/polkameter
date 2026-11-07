//! Redaction of credentials and endpoint secrets from everything PolkaMeter writes or prints.
//!
//! A [`Redactor`] knows the secrets of one run: the values of its credential profiles and the
//! sensitive parts of its endpoint URLs (user information, query values, path parameter values
//! and fragments). It finds every occurrence of every secret, overlapping ones included, and
//! replaces each run of covered bytes with `[redacted]`, so a secret that contains, starts or
//! ends another is never left half visible. Text that is already `[redacted]` stays as it is:
//! redacting twice gives what redacting once gives. Each secret is also looked for in the
//! escaped forms that JSON writers and Rust's `{:?}` produce, because plugins often log values
//! inside JSON strings.
//!
//! [`StreamRedactor`] applies the same redaction to a byte stream, such as a plugin's standard
//! error, without ever holding more than a bounded amount of it. [`endpoint`] redacts one URL
//! structurally, for the places that write an endpoint on purpose.

use aho_corasick::{AhoCorasick, MatchKind};
use serde_json::Value;
use std::{
	collections::HashSet,
	fmt,
	io::{Read as _, Write as _},
	path::Path,
	sync::{Arc, Mutex},
};

/// What every redacted secret becomes.
pub const REDACTED: &str = "[redacted]";

/// The shortest endpoint part (user information, query value or fragment) that is also redacted
/// on its own wherever it appears. Shorter parts, such as `v=1`, are redacted only inside the
/// endpoint URL itself: redacting every `1` in a run's files would destroy them. It is also the
/// shortest credential that is redacted from JSON object keys, and below it a credential gets a
/// warning.
pub const MIN_ENDPOINT_SECRET: usize = 8;

/// Longest partial line a [`StreamRedactor`] holds back waiting for its end. A longer line is
/// written in pieces, keeping back only what could still be the start of a secret.
pub const STREAM_LINE_LIMIT: usize = 64 * 1024;

/// How much of a file [`rewrite_file`] reads at a time.
const FILE_CHUNK: usize = 64 * 1024;

/// What a pattern is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
	/// A credential or endpoint part, in one of its forms.
	Secret,
	/// An endpoint URL, in one of its forms.
	Url,
	/// `[redacted]` itself, so a secret is never found inside text that is already redacted.
	Redacted,
}

/// One thing to find, and what replaces it.
struct Pattern {
	kind: Kind,
	/// For a URL, the URL as [`endpoint`] redacts it; for `[redacted]`, itself.
	replacement: Option<String>,
}

/// The patterns of one set, compiled once.
struct Patterns {
	automaton: AhoCorasick,
	patterns: Vec<Pattern>,
	/// Every pattern's bytes, for the partial matches at the end of a stream.
	needles: Vec<Vec<u8>>,
	longest: usize,
}

/// The patterns of one run.
struct Inner {
	all: Patterns,
	/// The patterns applied to JSON object keys: secrets of [`MIN_ENDPOINT_SECRET`] bytes or more
	/// and URLs.
	keys: Option<Patterns>,
	/// Every secret form, for a JSON object key that is a secret itself.
	exact: HashSet<String>,
}

/// Redacts a run's secrets from text, bytes and JSON values. Cheap to clone. Its `Debug` output
/// never shows the secrets.
#[derive(Clone, Default)]
pub struct Redactor(Option<Arc<Inner>>);

impl fmt::Debug for Redactor {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Redactor")
			.field("patterns", &self.0.as_ref().map_or(0, |p| p.all.patterns.len()))
			.finish()
	}
}

/// A run of covered bytes `start..end` and, when one endpoint URL (or one `[redacted]`) covers
/// it all, the pattern whose replacement it takes.
#[derive(Clone, Copy, Debug)]
struct Group {
	start: usize,
	end: usize,
	replacement: Option<usize>,
}

/// Adds the match `start..end` of `pattern` to `groups`, which are ordered and disjoint. Matches
/// must come in order of their end, as Aho-Corasick reports them, so only the last groups can
/// overlap the new one, and nothing but the groups is kept.
fn merge(groups: &mut Vec<Group>, start: usize, end: usize, replacement: Option<usize>) {
	let mut first: Option<Group> = None;
	while groups.last().is_some_and(|last| last.end > start) {
		first = groups.pop();
	}
	let group = match first {
		None => Group { start, end, replacement },
		// The new match reaches past the start of what it overlaps: it keeps its replacement
		// only if it is the whole run.
		Some(earlier) if earlier.start < start => Group {
			start: earlier.start,
			end,
			replacement: earlier.replacement.filter(|_| earlier.end == end),
		},
		Some(earlier) => Group {
			start,
			end,
			replacement: replacement
				.or(earlier.replacement.filter(|_| earlier.start == start && earlier.end == end)),
		},
	};
	groups.push(group);
}

impl Patterns {
	fn compile(mut entries: Vec<(String, Kind, Option<String>)>) -> Option<Self> {
		entries.retain(|(needle, ..)| !needle.is_empty());
		// A needle that is both a plain secret and part of a URL is redacted whole.
		entries.sort();
		entries.dedup_by(|a, b| a.0 == b.0);
		if entries.is_empty() {
			return None;
		}
		let needles: Vec<Vec<u8>> = entries.iter().map(|(n, ..)| n.as_bytes().to_vec()).collect();
		let automaton = AhoCorasick::builder()
			.match_kind(MatchKind::Standard)
			.build(&needles)
			.expect("redaction patterns compile");
		let longest = needles.iter().map(Vec::len).max().unwrap_or(0);
		let patterns = entries
			.into_iter()
			.map(|(_, kind, replacement)| Pattern { kind, replacement })
			.collect();
		Some(Self { automaton, patterns, needles, longest })
	}

	/// Merges into `groups` the matches in `hay`, which starts at offset `base`, that end after
	/// `after`.
	fn scan(&self, hay: &[u8], base: usize, after: usize, groups: &mut Vec<Group>) {
		for m in self.automaton.find_overlapping_iter(hay) {
			let (start, end) = (base + m.start(), base + m.end());
			if end <= after {
				continue;
			}
			let pattern = m.pattern().as_usize();
			let replacement = self.patterns[pattern].replacement.as_ref().map(|_| pattern);
			merge(groups, start, end, replacement);
		}
	}

	/// The covered runs of `hay`, in order. Overlapping matches are merged into one run.
	fn groups(&self, hay: &[u8]) -> Vec<Group> {
		let mut groups = Vec::new();
		self.scan(hay, 0, 0, &mut groups);
		groups
	}

	/// Appends `hay[from..to]` to `out`, redacted, where `hay` starts at offset `base` and
	/// `groups` are its covered runs. A run that reaches outside the range is still redacted
	/// in it, so a secret cut by a chunk boundary never shows.
	fn render(
		&self,
		hay: &[u8],
		base: usize,
		(from, to): (usize, usize),
		groups: &[Group],
		out: &mut Vec<u8>,
	) {
		let mut position = from;
		let first = groups.partition_point(|group| group.end <= from);
		for group in &groups[first..] {
			if group.start >= to {
				break;
			}
			let (start, end) = (group.start.max(from), group.end.min(to));
			out.extend_from_slice(&hay[position - base..start - base]);
			let whole = start == group.start && end == group.end;
			let replacement = group
				.replacement
				.filter(|_| whole)
				.and_then(|pattern| self.patterns[pattern].replacement.as_deref());
			out.extend_from_slice(replacement.unwrap_or(REDACTED).as_bytes());
			position = end;
		}
		out.extend_from_slice(&hay[position - base..to - base]);
	}

	fn bytes(&self, bytes: &[u8]) -> Vec<u8> {
		let mut out = Vec::with_capacity(bytes.len());
		self.render(bytes, 0, (0, bytes.len()), &self.groups(bytes), &mut out);
		out
	}

	fn text(&self, text: &str) -> String {
		// Secrets are valid UTF-8 and replace whole matches, so the result is valid UTF-8 too.
		String::from_utf8(self.bytes(text.as_bytes()))
			.unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
	}

	/// Where the longest end of `hay` that could still become a pattern starts: `hay[i..]` is a
	/// proper prefix of one. With `secrets_only`, only credential and endpoint patterns count.
	/// `hay.len()` when no end could.
	fn partial_start(&self, hay: &[u8], secrets_only: bool) -> usize {
		let mut start = hay.len();
		for (needle, pattern) in self.needles.iter().zip(&self.patterns) {
			if secrets_only && pattern.kind == Kind::Redacted {
				continue;
			}
			let longest = (needle.len() - 1).min(hay.len());
			// Only a longer partial match can move `start` back.
			for length in (hay.len() - start + 1..=longest).rev() {
				if hay.ends_with(&needle[..length]) {
					start = hay.len() - length;
					break;
				}
			}
		}
		start
	}
}

impl Redactor {
	/// A redactor for `secrets` (credential values) and `endpoints` (URLs whose user
	/// information, query values, path parameter values and fragments are secret). Empty
	/// secrets are ignored. A secret with whitespace around it, as read from a file, is also
	/// redacted without it.
	pub fn new<S: AsRef<str>, E: AsRef<str>>(
		secrets: impl IntoIterator<Item = S>,
		endpoints: impl IntoIterator<Item = E>,
	) -> Self {
		let mut plain = Vec::new();
		let mut urls = Vec::new();
		for secret in secrets {
			let secret = secret.as_ref();
			add_secret(&mut plain, secret);
			if secret.trim() != secret {
				add_secret(&mut plain, secret.trim());
			}
		}
		for url in endpoints {
			let url = url.as_ref();
			if endpoint(url) == url {
				continue;
			}
			for part in endpoint_secrets(url) {
				if part.len() >= MIN_ENDPOINT_SECRET {
					add_secret(&mut plain, &part);
				}
			}
			let mut forms = vec![url.to_owned()];
			// Clients may report a URL as the `url` crate normalizes it, e.g. with a `/` path.
			if let Ok(normalized) = reqwest::Url::parse(url).map(String::from)
				&& normalized != url
			{
				forms.push(normalized);
			}
			// Percent escapes may come back in either case.
			for form in forms.clone() {
				forms.extend(percent_cases(&form));
			}
			for form in forms {
				let redacted = endpoint(&form);
				urls.push((form, redacted));
			}
		}
		// A URL's redacted form must not show a secret either, e.g. a credential in its host.
		let protected = (REDACTED.to_owned(), Kind::Redacted, Some(REDACTED.to_owned()));
		let secrets_only = Patterns::compile(
			plain
				.iter()
				.map(|secret| (secret.clone(), Kind::Secret, None))
				.chain([protected.clone()])
				.collect(),
		);
		let exact: HashSet<String> = plain.iter().cloned().collect();
		let mut all: Vec<_> =
			plain.into_iter().map(|secret| (secret, Kind::Secret, None)).collect();
		for (url, redacted) in urls {
			let redacted = secrets_only.as_ref().map_or(redacted.clone(), |s| s.text(&redacted));
			// A URL inside a JSON string or debug output is escaped, and so is its redacted form.
			for escape in ESCAPES {
				all.push((escape(&url), Kind::Url, Some(escape(&redacted))));
			}
		}
		if all.is_empty() {
			return Self(None);
		}
		all.push(protected);
		let keys = Patterns::compile(
			all.iter()
				.filter(|(needle, kind, _)| {
					*kind != Kind::Secret || needle.len() >= MIN_ENDPOINT_SECRET
				})
				.cloned()
				.collect(),
		);
		let all = Patterns::compile(all).expect("at least one pattern");
		Self(Some(Arc::new(Inner { all, keys, exact })))
	}

	/// Whether there is nothing to redact.
	pub fn is_empty(&self) -> bool {
		self.0.is_none()
	}

	/// The length in bytes of the longest pattern, 0 when there is none.
	pub fn longest(&self) -> usize {
		self.0.as_ref().map_or(0, |p| p.all.longest)
	}

	/// `text` with every secret redacted.
	pub fn text(&self, text: &str) -> String {
		match &self.0 {
			Some(inner) => inner.all.text(text),
			None => text.to_owned(),
		}
	}

	/// `bytes` with every secret redacted. Bytes that are not UTF-8 are kept as they are.
	pub fn bytes(&self, bytes: &[u8]) -> Vec<u8> {
		match &self.0 {
			Some(inner) => inner.all.bytes(bytes),
			None => bytes.to_vec(),
		}
	}

	/// An object key, redacted: whole when it is a secret, else only the secrets of
	/// [`MIN_ENDPOINT_SECRET`] bytes or more and the endpoint URLs in it. Keys are mostly names,
	/// and a short secret such as `e` would destroy them.
	fn key(&self, key: &str) -> String {
		let Some(inner) = &self.0 else { return key.to_owned() };
		if inner.exact.contains(key) {
			return REDACTED.to_owned();
		}
		inner.keys.as_ref().map_or_else(|| key.to_owned(), |keys| keys.text(key))
	}

	/// Redacts every string in `value`. Object keys are redacted as [`Redactor::key`] says; two
	/// keys that become the same are both kept, the later one with ` (2)`, ` (3)`, … after it.
	pub fn value(&self, value: &mut Value) {
		if self.is_empty() {
			return;
		}
		match value {
			Value::String(text) => *text = self.text(text),
			Value::Array(items) => items.iter_mut().for_each(|item| self.value(item)),
			Value::Object(items) => {
				let mut out = serde_json::Map::new();
				for (key, mut item) in std::mem::take(items) {
					self.value(&mut item);
					let key = self.key(&key);
					let key = if out.contains_key(&key) {
						(2..)
							.map(|n| format!("{key} ({n})"))
							.find(|candidate| !out.contains_key(candidate))
							.expect("a free key")
					} else {
						key
					};
					out.insert(key, item);
				}
				*items = out;
			},
			_ => {},
		}
	}

	/// `value` as JSON with every string redacted.
	pub fn json(&self, value: &impl serde::Serialize) -> serde_json::Result<Value> {
		let mut value = serde_json::to_value(value)?;
		self.value(&mut value);
		Ok(value)
	}
}

/// `writer`, redacting every string of each record before it is written. Object keys are
/// redacted as [`Redactor::value`] says.
pub fn records(
	writer: polkameter_plugin_sdk::observers::files::JsonlWriter,
	redactor: &Redactor,
) -> polkameter_plugin_sdk::observers::files::JsonlWriter {
	if redactor.is_empty() {
		return writer;
	}
	let redactor = redactor.clone();
	writer.with_filter(move |value| redactor.value(value))
}

/// Whether a credential is short enough to redact text that is not it: under
/// [`MIN_ENDPOINT_SECRET`] bytes once whitespace around it is trimmed. Such a credential, like a
/// development key `//Alice`, still works and is still redacted.
pub fn is_short(secret: &str) -> bool {
	secret.trim().len() < MIN_ENDPOINT_SECRET
}

/// The public development accounts every Polkadot SDK dev chain funds.
const DEV_ACCOUNTS: [&str; 6] = ["Alice", "Bob", "Charlie", "Dave", "Eve", "Ferdie"];

/// Whether `secret` is a well-known development key URI: `//Alice`, `//Bob`, `//Charlie`,
/// `//Dave`, `//Eve` or `//Ferdie`, optionally followed by more `//x` or `/x` junctions, as in
/// `//Alice//stash`. These keys are public, so a short one needs no warning; it is still redacted.
pub fn is_dev_key(secret: &str) -> bool {
	let Some(rest) = secret.trim().strip_prefix("//") else { return false };
	let mut junctions = rest.split('/');
	if !junctions.next().is_some_and(|name| DEV_ACCOUNTS.contains(&name)) {
		return false;
	}
	// Each further junction is `/x` or `//x`: a non-empty part, at most one empty part before it.
	let mut empty = false;
	for part in junctions {
		if part.is_empty() {
			if empty {
				return false;
			}
			empty = true;
		} else {
			empty = false;
		}
	}
	!empty
}

/// Whether `c` must not reach a terminal or report as is: a control character, a bidirectional
/// override or isolate (which can make text read differently than it is), or U+FFFE/U+FFFF.
pub fn is_unsafe_char(c: char) -> bool {
	c.is_control()
		|| matches!(
			c,
			'\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{fffe}' | '\u{ffff}'
		)
}

/// `text` cut to at most `chars` characters, never inside a `[redacted]`: a cut one is dropped.
pub fn truncate(text: &str, chars: usize) -> String {
	let mut out: String = text.chars().take(chars).collect();
	if out.len() < text.len()
		&& let Some(open) = out.rfind('[')
		&& out.len() < open + REDACTED.len()
		&& text[open..].starts_with(REDACTED)
	{
		out.truncate(open);
	}
	out
}

/// The escaped forms of a text that are looked for besides the text itself.
const ESCAPES: [fn(&str) -> String; 14] = [
	|s| s.to_owned(),
	|s| json(s, Style { ascii: false, upper: false, html: Html::None }),
	|s| json(s, Style { ascii: true, upper: false, html: Html::None }),
	|s| json(s, Style { ascii: false, upper: true, html: Html::None }),
	|s| json(s, Style { ascii: true, upper: true, html: Html::None }),
	|s| json(s, Style { ascii: false, upper: false, html: Html::Go }),
	|s| json(s, Style { ascii: true, upper: false, html: Html::Go }),
	|s| json(s, Style { ascii: false, upper: true, html: Html::Go }),
	|s| json(s, Style { ascii: true, upper: true, html: Html::Go }),
	|s| json(s, Style { ascii: false, upper: false, html: Html::DotNet }),
	|s| json(s, Style { ascii: true, upper: false, html: Html::DotNet }),
	|s| json(s, Style { ascii: false, upper: true, html: Html::DotNet }),
	|s| json(s, Style { ascii: true, upper: true, html: Html::DotNet }),
	|s| {
		let debug = format!("{s:?}");
		debug[1..debug.len() - 1].to_owned()
	},
];

/// Adds `secret` and the forms it takes inside JSON strings and Rust debug output.
fn add_secret(out: &mut Vec<String>, secret: &str) {
	if !secret.is_empty() {
		out.extend(ESCAPES.iter().map(|escape| escape(secret)));
	}
}

/// Which characters a JSON writer also escapes so its output is safe inside HTML.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Html {
	None,
	/// Go's `encoding/json`: `<`, `>`, `&`, U+2028 and U+2029.
	Go,
	/// .NET's `System.Text.Json` default encoder: `<`, `>`, `&`, `'`, `+`, `` ` `` and `"`.
	DotNet,
}

/// How a JSON writer escapes a string.
#[derive(Clone, Copy)]
struct Style {
	/// Every non-ASCII character as `\uXXXX`, as Python's `json.dumps` writes by default.
	ascii: bool,
	/// Uppercase hexadecimal digits, as Jackson and .NET write.
	upper: bool,
	html: Html,
}

/// `text` inside a JSON string, without the quotes, as a writer of `style` escapes it.
fn json(text: &str, style: Style) -> String {
	let mut out = String::new();
	let unit = |out: &mut String, unit: u16| {
		if style.upper {
			out.push_str(&format!("\\u{unit:04X}"));
		} else {
			out.push_str(&format!("\\u{unit:04x}"));
		}
	};
	for c in text.chars() {
		let html = match style.html {
			Html::None => false,
			Html::Go => matches!(c, '<' | '>' | '&' | '\u{2028}' | '\u{2029}'),
			Html::DotNet => matches!(c, '<' | '>' | '&' | '\'' | '+' | '`' | '"'),
		};
		match c {
			_ if html => unit(&mut out, c as u16),
			'"' => out.push_str("\\\""),
			'\\' => out.push_str("\\\\"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			'\u{8}' => out.push_str("\\b"),
			'\u{c}' => out.push_str("\\f"),
			c if c < '\u{20}' => unit(&mut out, c as u16),
			c if c.is_ascii() || !style.ascii => out.push(c),
			c => {
				let mut units = [0; 2];
				for &mut u in c.encode_utf16(&mut units) {
					unit(&mut out, u);
				}
			},
		}
	}
	out
}

/// The parts of an endpoint URL, as written. The scheme is optional: a monitor URL such as
/// `127.0.0.1:9615/metrics?k=v` is read the same way.
struct UrlParts<'a> {
	/// The scheme and `://`, or nothing.
	scheme: &'a str,
	/// The user information, before the last `@` that comes before the query. A password may
	/// hold an unencoded `/` or `#`, so the `@` is looked for past them.
	userinfo: Option<&'a str>,
	/// Host and port.
	host: &'a str,
	path: &'a str,
	query: Option<&'a str>,
	fragment: Option<&'a str>,
}

fn split_url(url: &str) -> UrlParts<'_> {
	let scheme = url.find("://").map_or(0, |i| i + 3);
	let rest = &url[scheme..];
	let before_query = rest.find('?').unwrap_or(rest.len());
	let (userinfo, rest) = match rest[..before_query].rfind('@') {
		Some(at) => (Some(&rest[..at]), &rest[at + 1..]),
		None => (None, rest),
	};
	let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
	let (host, rest) = rest.split_at(host_end);
	let (rest, fragment) = match rest.split_once('#') {
		Some((rest, fragment)) => (rest, Some(fragment)),
		None => (rest, None),
	};
	let (path, query) = match rest.split_once('?') {
		Some((path, query)) => (path, Some(query)),
		None => (rest, None),
	};
	UrlParts { scheme: &url[..scheme], userinfo, host, path, query, fragment }
}

/// The value of each `name=value` parameter in `parameters`, split by `separator`; a bare one is
/// a value itself.
fn parameter_values(parameters: &str, separator: char) -> impl Iterator<Item = &str> {
	parameters
		.split(separator)
		.map(|parameter| parameter.split_once('=').map_or(parameter, |(_, value)| value))
}

/// `parameters` with each value redacted, keeping the names.
fn redact_parameters(parameters: &str, separator: char) -> String {
	let parameters: Vec<String> = parameters
		.split(separator)
		.map(|parameter| match parameter.split_once('=') {
			Some((name, "")) => format!("{name}="),
			Some((name, _)) => format!("{name}={REDACTED}"),
			// A bare value, such as `?token`, may be a key itself.
			None if parameter.is_empty() => String::new(),
			None => REDACTED.to_owned(),
		})
		.collect();
	parameters.join(&separator.to_string())
}

/// `url` with its user information, query values, path parameter values (`;name=value`) and
/// fragment redacted. The scheme, host, port, path and parameter names stay. A URL without a
/// scheme is read as `host:port/path?query`; text without any of these parts is returned as it
/// is.
///
/// ```
/// use polkameter::redact::endpoint;
/// assert_eq!(
///     endpoint("wss://user:pass@rpc.example:443/ws?apikey=k&v=1#t"),
///     "wss://[redacted]@rpc.example:443/ws?apikey=[redacted]&v=[redacted]#[redacted]"
/// );
/// assert_eq!(endpoint("127.0.0.1:9615/metrics?apikey=k"), "127.0.0.1:9615/metrics?apikey=[redacted]");
/// ```
pub fn endpoint(url: &str) -> String {
	let UrlParts { scheme, userinfo, host, path, query, fragment } = split_url(url);
	let mut out = String::from(scheme);
	if userinfo.is_some() {
		out.push_str(REDACTED);
		out.push('@');
	}
	out.push_str(host);
	let segments: Vec<String> = path
		.split('/')
		.map(|segment| match segment.split_once(';') {
			Some((name, parameters)) => format!("{name};{}", redact_parameters(parameters, ';')),
			None => segment.to_owned(),
		})
		.collect();
	out.push_str(&segments.join("/"));
	if let Some(query) = query {
		out.push('?');
		out.push_str(&redact_parameters(query, '&'));
	}
	if let Some(fragment) = fragment {
		out.push('#');
		if !fragment.is_empty() {
			out.push_str(REDACTED);
		}
	}
	out
}

/// The secret parts of `url`, as written, percent-decoded and with their percent escapes in
/// either case: its user information and password, every query and path parameter value and its
/// fragment.
fn endpoint_secrets(url: &str) -> Vec<String> {
	let UrlParts { userinfo, path, query, fragment, .. } = split_url(url);
	let mut parts = Vec::new();
	if let Some(userinfo) = userinfo {
		parts.push(userinfo);
		if let Some((_, password)) = userinfo.split_once(':') {
			parts.push(password);
		}
	}
	for segment in path.split('/') {
		if let Some((_, parameters)) = segment.split_once(';') {
			parts.extend(parameter_values(parameters, ';'));
		}
	}
	parts.extend(query.into_iter().flat_map(|query| parameter_values(query, '&')));
	parts.extend(fragment);
	let mut out = Vec::new();
	for part in parts.into_iter().filter(|part| !part.is_empty()) {
		out.push(part.to_owned());
		out.extend(percent_cases(part));
		let decoded = percent_decode(part);
		if decoded != part {
			out.push(decoded);
		}
	}
	out
}

/// `text` with the hexadecimal digits of its `%XX` escapes in upper and in lower case, those
/// that differ from it.
fn percent_cases(text: &str) -> Vec<String> {
	let bytes = text.as_bytes();
	let mut cases = [bytes.to_vec(), bytes.to_vec()];
	for i in 0..bytes.len() {
		if bytes[i] == b'%'
			&& bytes
				.get(i + 1..i + 3)
				.is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
		{
			for j in i + 1..i + 3 {
				cases[0][j] = bytes[j].to_ascii_uppercase();
				cases[1][j] = bytes[j].to_ascii_lowercase();
			}
		}
	}
	cases
		.into_iter()
		.filter_map(|case| String::from_utf8(case).ok())
		.filter(|case| case != text)
		.collect()
}

/// `text` with `%XX` escapes and `+` decoded, as servers read a query value.
fn percent_decode(text: &str) -> String {
	let bytes = text.as_bytes();
	let hex = |byte: u8| char::from(byte).to_digit(16);
	let mut out = Vec::with_capacity(bytes.len());
	let mut i = 0;
	while i < bytes.len() {
		match (
			bytes[i],
			bytes.get(i + 1).copied().and_then(hex),
			bytes.get(i + 2).copied().and_then(hex),
		) {
			(b'%', Some(high), Some(low)) => {
				out.push(u8::try_from(high * 16 + low).unwrap_or(b'%'));
				i += 3;
				continue;
			},
			(b'+', ..) => out.push(b' '),
			(byte, ..) => out.push(byte),
		}
		i += 1;
	}
	String::from_utf8_lossy(&out).into_owned()
}

/// Redacts a byte stream that arrives in chunks of any size, such as a plugin's standard error.
///
/// Whole lines are released as soon as they end. A secret may span chunks and lines: bytes that
/// could be the start of a secret are held back until the secret is complete or ruled out, and
/// the end of what was already released is kept as context, so the rest of a secret cut by a
/// release is still recognized. A line longer than [`STREAM_LINE_LIMIT`] is released in pieces,
/// holding back at most the longest secret's length minus one byte, so memory stays bounded.
/// Each chunk is scanned once, with that context: the cost is linear in the stream's length.
#[derive(Debug)]
pub struct StreamRedactor {
	redactor: Redactor,
	/// The context (the last released bytes, raw, at most the longest pattern's length minus
	/// one), then the bytes not released yet.
	buffer: Vec<u8>,
	/// The stream offset of `buffer[0]`.
	base: usize,
	/// The stream offset of the first byte not released yet.
	released: usize,
	/// The covered runs found so far that end after `released`, in stream offsets.
	groups: Vec<Group>,
	/// The stream offset just after the last line end seen.
	line_end: usize,
	limit: usize,
}

impl StreamRedactor {
	/// A stream redactor that holds partial lines up to [`STREAM_LINE_LIMIT`] bytes.
	pub fn new(redactor: Redactor) -> Self {
		Self::with_limit(redactor, STREAM_LINE_LIMIT)
	}

	/// A stream redactor that holds partial lines up to `limit` bytes.
	pub fn with_limit(redactor: Redactor, limit: usize) -> Self {
		Self {
			redactor,
			buffer: Vec::new(),
			base: 0,
			released: 0,
			groups: Vec::new(),
			line_end: 0,
			limit,
		}
	}

	/// The bytes not released yet.
	fn pending(&self) -> &[u8] {
		&self.buffer[self.released - self.base..]
	}

	/// Takes the next chunk and returns the redacted bytes that can be written now.
	pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
		let Some(inner) = self.redactor.0.clone() else { return chunk.to_vec() };
		let patterns = &inner.all;
		let end = self.base + self.buffer.len();
		self.buffer.extend_from_slice(chunk);
		if let Some(i) = chunk.iter().rposition(|&b| b == b'\n') {
			self.line_end = end + i + 1;
		}
		// Only a match that ends in the new bytes is new, and it starts at most the longest
		// pattern's length minus one before them.
		let window = end.saturating_sub(patterns.longest - 1).max(self.base);
		patterns.scan(&self.buffer[window - self.base..], window, end, &mut self.groups);

		let safe =
			(self.base + patterns.partial_start(&self.buffer, false)).saturating_sub(self.released);
		let over_limit = self.pending().len() > self.limit;
		let mut cut =
			if over_limit { safe } else { self.line_end.saturating_sub(self.released).min(safe) };
		// Do not cut through a secret that is already complete: release it whole, or, when the
		// line can wait, keep it whole for the next release.
		let at = self.released + cut;
		let straddling = self.groups.partition_point(|group| group.end <= at);
		if let Some(group) = self.groups.get(straddling).filter(|group| group.start < at) {
			cut = if !over_limit && group.start > self.released {
				group.start - self.released
			} else {
				group.end - self.released
			};
		}
		self.release(patterns, cut, false)
	}

	/// Returns the rest of the stream, redacted, once it has ended. What was held back because
	/// it could be the start of a secret becomes `[redacted]`: the stream ended, maybe because
	/// the writer was killed, in the middle of what may be a secret.
	pub fn finish(&mut self) -> Vec<u8> {
		let Some(inner) = self.redactor.0.clone() else { return Vec::new() };
		let cut = self.pending().len();
		self.release(&inner.all, cut, true)
	}

	/// Releases the next `cut` pending bytes. With `last`, a held end that could be the start of
	/// a secret becomes `[redacted]`.
	fn release(&mut self, patterns: &Patterns, cut: usize, last: bool) -> Vec<u8> {
		let mut out = Vec::new();
		if cut == 0 {
			return out;
		}
		let to = self.released + cut;
		let tail = if last {
			(self.base + patterns.partial_start(&self.buffer, true)).max(self.released)
		} else {
			to
		};
		patterns.render(&self.buffer, self.base, (self.released, tail), &self.groups, &mut out);
		if tail < to {
			out.extend_from_slice(REDACTED.as_bytes());
		}
		self.released = to;
		let keep = patterns.longest.saturating_sub(1);
		let base = to.saturating_sub(keep).max(self.base);
		self.buffer.drain(..base - self.base);
		self.base = base;
		let done = self.groups.partition_point(|group| group.end <= to);
		self.groups.drain(..done);
		out
	}
}

/// Redacts the file at `path` again, in bounded chunks: through a [`StreamRedactor`] into a
/// file next to it, which then replaces it. Most of the file is read without `lock`; what was
/// appended meanwhile is read with it held, up to the replacement, so a writer that takes the
/// lock and opens the file by path for each write never loses a write.
pub fn rewrite_file<G>(
	path: &Path,
	redactor: &Redactor,
	lock: impl FnOnce() -> G,
) -> std::io::Result<()> {
	if redactor.is_empty() {
		return Ok(());
	}
	let mut input = std::fs::File::open(path)?;
	let mut name = path.file_name().unwrap_or_default().to_owned();
	name.push(".redacting");
	let temporary = path.with_file_name(name);
	let mut options = std::fs::OpenOptions::new();
	options.create(true).write(true).truncate(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.mode(0o600);
	}
	let mut output = std::io::BufWriter::new(options.open(&temporary)?);
	let mut stream = StreamRedactor::new(redactor.clone());
	let mut chunk = vec![0; FILE_CHUNK];
	let mut copy = |input: &mut std::fs::File,
	                output: &mut std::io::BufWriter<std::fs::File>,
	                last: bool|
	 -> std::io::Result<()> {
		loop {
			match input.read(&mut chunk)? {
				0 if last => return output.write_all(&stream.finish()),
				0 => return Ok(()),
				read => output.write_all(&stream.push(&chunk[..read]))?,
			}
		}
	};
	let result = (|| {
		copy(&mut input, &mut output, false)?;
		let _guard = lock();
		copy(&mut input, &mut output, true)?;
		output.flush()?;
		std::fs::rename(&temporary, path)
	})();
	if result.is_err() {
		let _ = std::fs::remove_file(&temporary);
	}
	result
}

/// The redactors of the runs in progress in this process, for diagnostics written outside any
/// one run's files.
static ACTIVE: Mutex<Vec<(u64, Redactor)>> = Mutex::new(Vec::new());

/// Keeps a run's redactor applied to [`diagnostic`] text until it is dropped.
#[must_use = "the redactor is removed when this is dropped"]
pub struct Active(u64);

impl Drop for Active {
	fn drop(&mut self) {
		let mut active = ACTIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		active.retain(|(id, _)| *id != self.0);
	}
}

/// Applies `redactor` to [`diagnostic`] text while the returned guard lives.
pub fn activate(redactor: &Redactor) -> Active {
	static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
	let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	if !redactor.is_empty() {
		let mut active = ACTIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		active.push((id, redactor.clone()));
	}
	Active(id)
}

/// `text` redacted with the redactor of every run in progress.
pub fn diagnostic(text: &str) -> String {
	let active = ACTIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
	active.iter().fold(text.to_owned(), |text, (_, redactor)| redactor.text(&text))
}

/// `eprintln!`, redacted with the redactor of every run in progress: for the diagnostics a run
/// prints on standard error.
macro_rules! diag {
	($($arg:tt)*) => {
		eprintln!("{}", $crate::redact::diagnostic(&format!($($arg)*)))
	};
}
pub(crate) use diag;

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn well_known_development_keys_are_recognised() {
		for key in [
			"//Alice",
			"//Bob",
			"//Charlie",
			"//Dave",
			"//Eve",
			"//Ferdie",
			" //Alice ",
			"//Alice//stash",
			"//Bob/0",
			"//Eve//1/x",
		] {
			assert!(is_dev_key(key), "{key}");
		}
		for key in [
			"",
			"Alice",
			"/Alice",
			"//alice",
			"//Mallory",
			"//Alicee",
			"//Alice/",
			"//Alice//",
			"//Alice///pw",
			"//Bob///",
			"abc",
			"//Alice:x",
		] {
			assert!(!is_dev_key(key), "{key}");
		}
	}

	fn secrets(secrets: &[&str]) -> Redactor {
		Redactor::new(secrets, None::<&str>)
	}

	#[test]
	fn longest_match_wins_for_prefixes_suffixes_and_substrings() {
		let redactor = secrets(&["abc", "abcdef", "def", "cdefgh"]);
		assert_eq!(redactor.text("x abcdef y"), "x [redacted] y");
		assert_eq!(redactor.text("x abcdefgh y"), "x [redacted] y");
		assert_eq!(redactor.text("abc def abcabc"), "[redacted] [redacted] [redacted][redacted]");
		// Order of the secrets makes no difference.
		let reversed = secrets(&["cdefgh", "def", "abcdef", "abc"]);
		assert_eq!(reversed.text("x abcdefgh y"), "x [redacted] y");
		// One secret inside another, and overlapping ones.
		assert_eq!(secrets(&["token", "my-token-value"]).text("my-token-value"), "[redacted]");
		assert_eq!(secrets(&["12345", "34567"]).text("a1234567b"), "a[redacted]b");
		// A later, longer match that starts before several earlier ones swallows them all.
		assert_eq!(secrets(&["b", "d", "abcde"]).text("xabcdex b"), "x[redacted]x [redacted]");
	}

	#[test]
	fn overlapping_matches_are_merged_without_keeping_them_all() {
		// Every position starts 32 overlapping matches; only the merged runs are kept.
		let all: Vec<String> = (1..=32).map(|n| "a".repeat(n)).collect();
		let redactor = Redactor::new(&all, None::<&str>);
		let hay = vec![b'a'; 1 << 20];
		let groups = redactor.0.as_ref().unwrap().all.groups(&hay);
		assert_eq!(groups.len(), 1, "{:?}", &groups[..groups.len().min(4)]);
		assert_eq!(redactor.bytes(&hay), REDACTED.as_bytes());
	}

	#[test]
	fn escaped_forms_are_redacted() {
		let secret = "pa\"ss\\word\nwith é and 🔑 <&> 'q' a+b `t`";
		let redactor = secrets(&[secret]);
		let json = serde_json::to_string(&json!({ "key": secret })).unwrap();
		assert_eq!(redactor.text(&json), r#"{"key":"[redacted]"}"#);
		let python = r#"{"key": "pa\"ss\\word\nwith \u00e9 and \ud83d\udd11 <&> 'q' a+b `t`"}"#;
		assert_eq!(redactor.text(python), r#"{"key": "[redacted]"}"#);
		// Jackson and .NET write uppercase hexadecimal digits.
		let upper = r#""pa\"ss\\word\nwith \u00E9 and \uD83D\uDD11 <&> 'q' a+b `t`""#;
		assert_eq!(redactor.text(upper), "\"[redacted]\"");
		// Go escapes `<`, `>` and `&` for HTML.
		let go = r#""pa\"ss\\word\nwith é and 🔑 \u003c\u0026\u003e 'q' a+b `t`""#;
		assert_eq!(redactor.text(go), "\"[redacted]\"");
		// .NET also escapes quotes, `+` and backticks, and every non-ASCII character.
		let dotnet = r#""pa\u0022ss\\word\nwith \u00E9 and \uD83D\uDD11 \u003C\u0026\u003E \u0027q\u0027 a\u002Bb \u0060t\u0060""#;
		assert_eq!(redactor.text(dotnet), "\"[redacted]\"");
		assert_eq!(redactor.text(&format!("{secret:?}")), "\"[redacted]\"");
		assert_eq!(redactor.text(&format!("raw {secret} end")), "raw [redacted] end");
	}

	#[test]
	fn values_are_redacted_and_keys_only_for_long_or_whole_secrets() {
		let redactor = secrets(&["s3cret-value", "e"]);
		let mut value = json!({
			"s3cret-value": ["a s3cret-value", 1, {"k": "s3cret-value"}],
			"key with s3cret-value": "e",
			"e": "name",
			"name": "x",
		});
		redactor.value(&mut value);
		assert_eq!(
			value,
			json!({
				"[redacted] (2)": ["a [redacted]", 1, {"k": "[redacted]"}],
				"key with [redacted]": "[redacted]",
				// The key is the short secret itself; another key is not touched by it.
				"[redacted]": "nam[redacted]",
				"name": "x",
			})
		);
		assert!(!format!("{redactor:?}").contains("s3cret"));
		assert_eq!(Redactor::default().text("s3cret"), "s3cret");
		assert!(secrets(&[""]).is_empty());
	}

	#[test]
	fn redacting_twice_is_redacting_once() {
		let redactor = secrets(&["e", "d]", "[r", "act", "s3cret-value"]);
		for text in [
			"some text with s3cret-value and e",
			"[redacted] stays",
			"a [redacted][redacted] b",
			"redacted]",
			"edge [red",
		] {
			let once = redactor.text(text);
			assert_eq!(redactor.text(&once), once, "{text}");
		}
		// A short secret never eats into the token.
		assert_eq!(secrets(&["e"]).text("[redacted] e"), "[redacted] [redacted]");
		let url = Redactor::new(["e"], ["wss://host/?k=v"]);
		let once = url.text("at wss://host/?k=v");
		assert_eq!(url.text(&once), once);
	}

	#[test]
	fn secrets_with_whitespace_around_them_are_also_redacted_trimmed() {
		let redactor = secrets(&["file-secret\n"]);
		assert_eq!(redactor.text("got file-secret here"), "got [redacted] here");
		assert_eq!(redactor.text("got file-secret\n"), "got [redacted]");
		assert!(is_short("//Alice\n") && !is_short("long enough"));
	}

	#[test]
	fn truncation_never_cuts_a_redacted_token() {
		assert_eq!(truncate("abc [redacted] def", 8), "abc ");
		assert_eq!(truncate("abc [redacted] def", 14), "abc [redacted]");
		assert_eq!(truncate("abc [reda", 8), "abc [red");
		assert_eq!(truncate("short", 80), "short");
	}

	#[test]
	fn non_utf8_bytes_are_kept() {
		let redactor = secrets(&["key"]);
		assert_eq!(redactor.bytes(b"\xff key \xfe"), b"\xff [redacted] \xfe");
	}

	#[test]
	fn endpoints_keep_scheme_host_port_and_path() {
		for (url, expected) in [
			("wss://rpc.example/ws", "wss://rpc.example/ws"),
			("ws://127.0.0.1:9944", "ws://127.0.0.1:9944"),
			("wss://host/?apikey=abc", "wss://host/?apikey=[redacted]"),
			("wss://host?apikey=abc&network=x", "wss://host?apikey=[redacted]&network=[redacted]"),
			("https://user:pass@host:8443/p", "https://[redacted]@host:8443/p"),
			("https://user@host", "https://[redacted]@host"),
			("https://a@b:c@host/x?y", "https://[redacted]@host/x?[redacted]"),
			("wss://host/?a=&&b", "wss://host/?a=&&[redacted]"),
			("wss://host/p#frag", "wss://host/p#[redacted]"),
			("wss://host/p?x=1#", "wss://host/p?x=[redacted]#"),
			// An unencoded `/` or `#` in the password.
			("wss://user:pa/ss#w@host/ws", "wss://[redacted]@host/ws"),
			// Path parameters.
			("wss://host/ws;apikey=abc;v/x", "wss://host/ws;apikey=[redacted];[redacted]/x"),
			// No scheme, as in a topology's `prometheus_uri`.
			("127.0.0.1:9615/metrics?apikey=k", "127.0.0.1:9615/metrics?apikey=[redacted]"),
			("user:pw@10.0.0.1:9615", "[redacted]@10.0.0.1:9615"),
			("127.0.0.1:9615/metrics", "127.0.0.1:9615/metrics"),
			("not a url", "not a url"),
		] {
			assert_eq!(endpoint(url), expected, "{url}");
		}
	}

	#[test]
	fn endpoint_secrets_are_redacted_in_free_text() {
		let url = "wss://alice:hunter2hunter2@rpc.example:443/ws?apikey=K3y%2FValue12&v=1";
		let redactor = Redactor::new(None::<&str>, [url]);
		let redacted = "wss://[redacted]@rpc.example:443/ws?apikey=[redacted]&v=[redacted]";
		assert_eq!(redactor.text(url), redacted);
		assert_eq!(
			redactor.text(&format!("cannot connect to {url}: refused")),
			format!("cannot connect to {redacted}: refused")
		);
		// Long parts are secrets on their own, also percent-decoded and in either escape case;
		// short ones are not.
		assert_eq!(
			redactor.text("key K3y/Value12, K3y%2fValue12, pass hunter2hunter2, v 1"),
			"key [redacted], [redacted], pass [redacted], v 1"
		);
		// The whole URL with its escapes in the other case.
		assert_eq!(redactor.text(&url.replace("%2F", "%2f")), redacted);
		// The `url` crate's normal form, with a `/` path, is recognized too.
		let short = Redactor::new(None::<&str>, ["wss://host?k=v"]);
		assert_eq!(short.text("error at wss://host/?k=v"), "error at wss://host/?k=[redacted]");
		// A URL without secrets adds nothing.
		assert!(Redactor::new(None::<&str>, ["wss://host:443/path"]).is_empty());
	}

	#[test]
	fn scheme_less_endpoints_are_redacted() {
		let url = "127.0.0.1:9615/metrics?apikey=k";
		let redactor = Redactor::new(None::<&str>, [url]);
		assert_eq!(
			redactor.text(&format!("cannot read {url}")),
			"cannot read 127.0.0.1:9615/metrics?apikey=[redacted]"
		);
	}

	#[test]
	fn escaped_endpoints_are_redacted_whole() {
		let url = r#"wss://host/?k=a"b"#;
		let redactor = Redactor::new(None::<&str>, [url]);
		let json = serde_json::to_string(&json!({ "url": url })).unwrap();
		assert_eq!(redactor.text(&json), r#"{"url":"wss://host/?k=[redacted]"}"#);
		assert_eq!(redactor.text(&format!("{url:?}")), r#""wss://host/?k=[redacted]""#);
		// The kept part of the URL is escaped in the replacement as it was in the text.
		let url = r#"wss://ho"st/?k=v"#;
		let json = serde_json::to_string(url).unwrap();
		assert_eq!(
			Redactor::new(None::<&str>, [url]).text(&json),
			r#""wss://ho\"st/?k=[redacted]""#
		);
	}

	#[test]
	fn a_credential_overlapping_an_endpoint_is_redacted_whole() {
		let url = "wss://host/?k=abc";
		let redactor = Redactor::new(["abcdefgh"], [url]);
		assert_eq!(redactor.text(url), "wss://host/?k=[redacted]");
		assert_eq!(redactor.text("wss://host/?k=abcdefgh"), "[redacted]");
		// A credential inside the part of the URL that stays is redacted from it too.
		let redactor = Redactor::new(["secret-host"], ["wss://secret-host/?k=v"]);
		assert_eq!(redactor.text("wss://secret-host/?k=v"), "wss://[redacted]/?k=[redacted]");
	}

	#[test]
	fn diagnostics_are_redacted_while_a_run_is_active() {
		let marker = "diagnostic-secret-marker";
		assert_eq!(diagnostic(marker), marker);
		let active = activate(&secrets(&[marker]));
		assert_eq!(diagnostic(&format!("x {marker}")), "x [redacted]");
		drop(active);
		assert_eq!(diagnostic(marker), marker);
	}

	#[test]
	fn rewriting_a_file_redacts_it_in_chunks() {
		let dir = std::env::temp_dir().join(format!("polkameter-rewrite-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("stderr.log");
		// A secret across the chunk boundary.
		let mut text = "x".repeat(FILE_CHUNK - 3);
		text.push_str("secret-value\nend secret-value\n");
		std::fs::write(&path, &text).unwrap();
		let redactor = secrets(&["secret-value"]);
		rewrite_file(&path, &redactor, || ()).unwrap();
		let out = std::fs::read_to_string(&path).unwrap();
		assert_eq!(out, redactor.text(&text));
		assert!(!out.contains("secret"));
		assert!(!dir.join("stderr.log.redacting").exists());
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn a_write_appended_during_a_rewrite_is_kept_and_redacted() {
		let dir =
			std::env::temp_dir().join(format!("polkameter-rewrite-append-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("stderr.log");
		std::fs::write(&path, "first secret-value\n").unwrap();
		let redactor = secrets(&["secret-value"]);
		// A streamed write lands after the unlocked part of the pass, before the lock is held.
		rewrite_file(&path, &redactor, || {
			let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
			file.write_all(b"late secret-value\n").unwrap();
		})
		.unwrap();
		assert_eq!(std::fs::read_to_string(&path).unwrap(), "first [redacted]\nlate [redacted]\n");
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn records_are_redacted_before_they_are_written() {
		use polkameter_plugin_sdk::observers::files::{RunDir, ScrapeRecord};
		let dir = std::env::temp_dir().join(format!("polkameter-records-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let run = RunDir::open(&dir);
		let redactor = Redactor::new(["MetricSecret-0123"], ["127.0.0.1:9615/metrics?k=v"]);
		let mut out = records(run.jsonl("scrapes.jsonl").unwrap(), &redactor);
		out.write(&ScrapeRecord {
			t: 1,
			job: "collator".into(),
			instance: "a".into(),
			text: Some("label{key=\"MetricSecret-0123\"} 1".into()),
			error: Some("cannot read 127.0.0.1:9615/metrics?k=v".into()),
		})
		.unwrap();
		out.finish().unwrap();
		let text = std::fs::read_to_string(dir.join("scrapes.jsonl")).unwrap();
		assert!(!text.contains("MetricSecret") && !text.contains("k=v"), "{text}");
		assert!(text.contains("127.0.0.1:9615/metrics?k=[redacted]"), "{text}");
		std::fs::remove_dir_all(dir).unwrap();
	}

	/// Feeds `chunks` to a stream redactor and returns everything it wrote.
	fn stream(redactor: &Redactor, limit: usize, chunks: &[&[u8]]) -> (Vec<u8>, Vec<usize>) {
		let mut stream = StreamRedactor::with_limit(redactor.clone(), limit);
		let mut out = Vec::new();
		let mut held = Vec::new();
		for chunk in chunks {
			out.extend(stream.push(chunk));
			held.push(stream.pending().len());
		}
		out.extend(stream.finish());
		(out, held)
	}

	/// `text` with runs of adjacent `[redacted]` collapsed into one.
	fn collapsed(text: &str) -> String {
		let twice = format!("{REDACTED}{REDACTED}");
		let mut text = text.to_owned();
		while text.contains(&twice) {
			text = text.replace(&twice, REDACTED);
		}
		text
	}

	#[test]
	fn stream_matches_whole_text_however_it_is_split() {
		let redactor = secrets(&["abc", "abcdef", "cdefgh", "multi\nline"]);
		let text = "1 abcdefgh 2 abc 3 xabcdefx 4 multi\nline 5 abcab\n6 cdefg 7\n";
		let expected = redactor.text(text);
		assert!(!expected.contains("bc"), "{expected}");
		for limit in [1, 4, 16, 1024] {
			for size in 1..text.len() {
				let chunks: Vec<&[u8]> = text.as_bytes().chunks(size).collect();
				let (out, _) = stream(&redactor, limit, &chunks);
				let out = String::from_utf8(out).unwrap();
				for secret in ["abc", "cdefgh", "multi", "line"] {
					assert!(!out.contains(secret), "limit {limit}, size {size}: {out}");
				}
				assert_eq!(collapsed(&out), expected, "limit {limit}, size {size}");
			}
		}
	}

	#[test]
	fn stream_releases_whole_lines_and_holds_partial_ones() {
		let redactor = secrets(&["secret"]);
		let mut stream = StreamRedactor::new(redactor);
		assert_eq!(stream.push(b"first line\nsecond"), b"first line\n");
		assert_eq!(stream.push(b" line with a sec"), b"");
		assert_eq!(stream.push(b"ret\n"), b"second line with a [redacted]\n");
		assert_eq!(stream.push(b"tail"), b"");
		assert_eq!(stream.finish(), b"tail");
		// Without secrets, bytes go straight through.
		assert_eq!(StreamRedactor::new(Redactor::default()).push(b"no newline"), b"no newline");
	}

	#[test]
	fn a_stream_cut_inside_what_may_be_a_secret_ends_redacted() {
		let redactor = secrets(&["SeedPhraseSensitive"]);
		let mut stream = StreamRedactor::new(redactor.clone());
		assert_eq!(stream.push(b"line\nkilled while writing SeedPhraseSen"), b"line\n");
		assert_eq!(stream.finish(), b"killed while writing [redacted]");
		// An over-long line keeps back only the possible start of the secret.
		let mut stream = StreamRedactor::with_limit(redactor, 4);
		let mut out = stream.push(b"aaaaaaaa SeedPhr");
		out.extend(stream.finish());
		assert_eq!(out, b"aaaaaaaa [redacted]");
	}

	#[test]
	fn streaming_a_long_line_in_small_chunks_scans_each_byte_a_bounded_number_of_times() {
		let redactor = secrets(&["0123456789-secret"]);
		let mut stream = StreamRedactor::new(redactor);
		let line = "x".repeat(STREAM_LINE_LIMIT - 1);
		let started = std::time::Instant::now();
		for chunk in line.as_bytes().chunks(1) {
			assert!(stream.push(chunk).is_empty());
		}
		assert_eq!(stream.push(b"\n").len(), STREAM_LINE_LIMIT);
		// Quadratic rescanning of the held line would take minutes here.
		assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
		assert!(stream.buffer.len() < 64, "{}", stream.buffer.len());
	}

	#[test]
	fn over_long_lines_hold_back_at_most_the_longest_secret_minus_one() {
		let secret = "0123456789-secret"; // gitleaks:allow (made-up test credential)
		let redactor = secrets(&[secret]);
		let longest = redactor.longest();
		let line = format!("{}{secret}{}", "x".repeat(10_000), "y".repeat(10_000)).repeat(20);
		let chunks: Vec<&[u8]> = line.as_bytes().chunks(4096).collect();
		let (out, held) = stream(&redactor, 1024, &chunks);
		assert!(held.iter().all(|&held| held < longest), "{held:?}");
		let out = String::from_utf8(out).unwrap();
		assert!(!out.contains("secret") && !out.contains("0123"));
		assert_eq!(out, redactor.text(&line));
		// A secret split across reads of an over-long line.
		let (out, _) = stream(&redactor, 4, &[b"aaaaaaaa0123456789-se", b"cret bbbb"]);
		assert_eq!(out, b"aaaaaaaa[redacted] bbbb");
	}
}
