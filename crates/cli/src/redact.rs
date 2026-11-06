//! Redaction of credentials and endpoint secrets from everything PolkaMeter writes or prints.
//!
//! A [`Redactor`] knows the secrets of one run: the values of its credential profiles and the
//! sensitive parts of its endpoint URLs (user information, query values and fragments). It finds
//! every occurrence of every secret, overlapping ones included, and replaces each run of covered
//! bytes with `[redacted]`, so a secret that contains, starts or ends another is never left half
//! visible. Each secret is also looked for in the escaped forms that JSON writers and Rust's
//! `{:?}` produce, because plugins often log values inside JSON strings.
//!
//! [`StreamRedactor`] applies the same redaction to a byte stream, such as a plugin's standard
//! error, without ever holding more than a bounded amount of it. [`endpoint`] redacts one URL
//! structurally, for the places that write an endpoint on purpose.

use aho_corasick::{AhoCorasick, MatchKind};
use serde_json::Value;
use std::{fmt, sync::Arc};

/// What every redacted secret becomes.
pub const REDACTED: &str = "[redacted]";

/// The shortest endpoint part (user information, query value or fragment) that is also redacted
/// on its own wherever it appears. Shorter parts, such as `v=1`, are redacted only inside the
/// endpoint URL itself: redacting every `1` in a run's files would destroy them.
pub const MIN_ENDPOINT_SECRET: usize = 8;

/// Longest partial line a [`StreamRedactor`] holds back waiting for its end. A longer line is
/// written in pieces, keeping back only what could still be the start of a secret.
pub const STREAM_LINE_LIMIT: usize = 64 * 1024;

/// One thing to find, and what replaces it.
struct Pattern {
	/// An endpoint URL, written as [`endpoint`] redacts it, or `None` for a plain secret.
	replacement: Option<String>,
}

/// The patterns of one run, compiled once.
struct Patterns {
	automaton: AhoCorasick,
	patterns: Vec<Pattern>,
	/// Every pattern's bytes, for the partial matches at the end of a stream.
	needles: Vec<Vec<u8>>,
	longest: usize,
}

/// Redacts a run's secrets from text, bytes and JSON values. Cheap to clone. Its `Debug` output
/// never shows the secrets.
#[derive(Clone, Default)]
pub struct Redactor(Option<Arc<Patterns>>);

impl fmt::Debug for Redactor {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Redactor")
			.field("patterns", &self.0.as_ref().map_or(0, |p| p.patterns.len()))
			.finish()
	}
}

/// A run of covered bytes `start..end` and, when one endpoint URL covers it all, that URL's
/// redacted form.
struct Group<'a> {
	start: usize,
	end: usize,
	replacement: Option<&'a str>,
}

impl Redactor {
	/// A redactor for `secrets` (credential values) and `endpoints` (URLs whose user
	/// information, query values and fragments are secret). Empty secrets are ignored.
	pub fn new<S: AsRef<str>, E: AsRef<str>>(
		secrets: impl IntoIterator<Item = S>,
		endpoints: impl IntoIterator<Item = E>,
	) -> Self {
		let mut plain = Vec::new();
		let mut urls = Vec::new();
		for secret in secrets {
			add_secret(&mut plain, secret.as_ref());
		}
		for url in endpoints {
			let url = url.as_ref();
			let redacted = endpoint(url);
			if redacted == url {
				continue;
			}
			for part in endpoint_secrets(url) {
				if part.len() >= MIN_ENDPOINT_SECRET {
					add_secret(&mut plain, &part);
				}
			}
			urls.push((url.to_owned(), redacted.clone()));
			// Clients may report a URL as the `url` crate normalizes it, e.g. with a `/` path.
			if let Ok(normalized) = reqwest::Url::parse(url).map(String::from)
				&& normalized != url
			{
				urls.push((normalized.clone(), endpoint(&normalized)));
			}
		}
		// A URL's redacted form must not show a secret either, e.g. a credential in its host.
		let secrets_only = Self::compile(plain.iter().map(|s| (s.clone(), None)).collect());
		let mut all: Vec<_> = plain.into_iter().map(|s| (s, None)).collect();
		for (url, redacted) in urls {
			let redacted = secrets_only.text(&redacted);
			all.push((url, Some(redacted)));
		}
		Self::compile(all)
	}

	fn compile(mut entries: Vec<(String, Option<String>)>) -> Self {
		entries.retain(|(needle, _)| !needle.is_empty());
		entries.sort();
		entries.dedup_by(|a, b| a.0 == b.0);
		if entries.is_empty() {
			return Self(None);
		}
		let needles: Vec<Vec<u8>> = entries.iter().map(|(n, _)| n.as_bytes().to_vec()).collect();
		let automaton = AhoCorasick::builder()
			.match_kind(MatchKind::Standard)
			.build(&needles)
			.expect("redaction patterns compile");
		let longest = needles.iter().map(Vec::len).max().unwrap_or(0);
		let patterns =
			entries.into_iter().map(|(_, replacement)| Pattern { replacement }).collect();
		Self(Some(Arc::new(Patterns { automaton, patterns, needles, longest })))
	}

	/// Whether there is nothing to redact.
	pub fn is_empty(&self) -> bool {
		self.0.is_none()
	}

	/// The length in bytes of the longest pattern, 0 when there is none.
	pub fn longest(&self) -> usize {
		self.0.as_ref().map_or(0, |p| p.longest)
	}

	/// `text` with every secret redacted.
	pub fn text(&self, text: &str) -> String {
		if self.is_empty() {
			return text.to_owned();
		}
		// Secrets are valid UTF-8 and replace whole matches, so the result is valid UTF-8 too.
		String::from_utf8(self.bytes(text.as_bytes()))
			.unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
	}

	/// `bytes` with every secret redacted. Bytes that are not UTF-8 are kept as they are.
	pub fn bytes(&self, bytes: &[u8]) -> Vec<u8> {
		let mut out = Vec::with_capacity(bytes.len());
		self.render(bytes, 0, bytes.len(), &mut out);
		out
	}

	/// Redacts every string in `value`, object keys included.
	pub fn value(&self, value: &mut Value) {
		if self.is_empty() {
			return;
		}
		match value {
			Value::String(text) => *text = self.text(text),
			Value::Array(items) => items.iter_mut().for_each(|item| self.value(item)),
			Value::Object(items) => {
				*items = std::mem::take(items)
					.into_iter()
					.map(|(key, mut item)| {
						self.value(&mut item);
						(self.text(&key), item)
					})
					.collect();
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

	/// The covered runs of `hay`, in order. Overlapping matches are merged into one run.
	fn groups(&self, hay: &[u8]) -> Vec<Group<'_>> {
		let Some(patterns) = &self.0 else { return Vec::new() };
		let mut matches: Vec<_> = patterns
			.automaton
			.find_overlapping_iter(hay)
			.map(|m| (m.start(), m.end(), m.pattern().as_usize()))
			.collect();
		matches.sort_unstable_by_key(|&(start, end, _)| (start, std::cmp::Reverse(end)));
		let mut groups: Vec<Group<'_>> = Vec::new();
		for (start, end, pattern) in matches {
			let replacement = patterns.patterns[pattern].replacement.as_deref();
			match groups.last_mut() {
				Some(group) if start < group.end => {
					// Matches are sorted by start, then longest first: a match inside the group's
					// first URL keeps its replacement, anything reaching past it does not.
					if end > group.end {
						group.end = end;
						group.replacement = None;
					}
				},
				_ => groups.push(Group { start, end, replacement }),
			}
		}
		groups
	}

	/// Appends `hay[from..to]` to `out`, redacted. Matches may reach outside the range: the
	/// part inside it is still redacted, so a secret cut by a chunk boundary never shows.
	fn render(&self, hay: &[u8], from: usize, to: usize, out: &mut Vec<u8>) {
		let mut position = from;
		for group in self.groups(hay) {
			if group.end <= from || group.start >= to {
				continue;
			}
			let (start, end) = (group.start.max(from), group.end.min(to));
			out.extend_from_slice(&hay[position..start]);
			let whole = start == group.start && end == group.end;
			out.extend_from_slice(
				group.replacement.filter(|_| whole).unwrap_or(REDACTED).as_bytes(),
			);
			position = end;
		}
		out.extend_from_slice(&hay[position..to]);
	}

	/// Where the longest end of `hay` that could still become a secret starts: `hay[i..]` is a
	/// proper prefix of a pattern. `hay.len()` when no end could.
	fn partial_start(&self, hay: &[u8]) -> usize {
		let Some(patterns) = &self.0 else { return hay.len() };
		let mut start = hay.len();
		for needle in &patterns.needles {
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

/// Adds `secret` and the forms it takes inside JSON strings and Rust debug output.
fn add_secret(out: &mut Vec<String>, secret: &str) {
	if secret.is_empty() {
		return;
	}
	out.push(secret.to_owned());
	if let Ok(json) = serde_json::to_string(secret) {
		out.push(json[1..json.len() - 1].to_owned());
	}
	out.push(json_ascii(secret));
	let debug = format!("{secret:?}");
	out.push(debug[1..debug.len() - 1].to_owned());
}

/// `secret` escaped as a JSON writer that escapes all non-ASCII text writes it (Python's
/// `json.dumps` does by default).
fn json_ascii(secret: &str) -> String {
	let mut out = String::new();
	for c in secret.chars() {
		match c {
			'"' => out.push_str("\\\""),
			'\\' => out.push_str("\\\\"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			'\u{8}' => out.push_str("\\b"),
			'\u{c}' => out.push_str("\\f"),
			c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
			c => {
				let mut units = [0; 2];
				for unit in c.encode_utf16(&mut units) {
					out.push_str(&format!("\\u{unit:04x}"));
				}
			},
		}
	}
	out
}

/// The parts of a `scheme://` URL, as written.
struct UrlParts<'a> {
	/// The scheme and `://`.
	scheme: &'a str,
	/// User information, host and port.
	authority: &'a str,
	path: &'a str,
	query: Option<&'a str>,
	fragment: Option<&'a str>,
}

/// The parts of `url`, or `None` when it has no scheme.
fn split_url(url: &str) -> Option<UrlParts<'_>> {
	let scheme = url.find("://")? + 3;
	let rest = &url[scheme..];
	let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
	let (authority, rest) = rest.split_at(authority_end);
	let (rest, fragment) = match rest.split_once('#') {
		Some((rest, fragment)) => (rest, Some(fragment)),
		None => (rest, None),
	};
	let (path, query) = match rest.split_once('?') {
		Some((path, query)) => (path, Some(query)),
		None => (rest, None),
	};
	Some(UrlParts { scheme: &url[..scheme], authority, path, query, fragment })
}

/// `url` with its user information, query values and fragment redacted. The scheme, host, port,
/// path and query parameter names stay. Text that is not a `scheme://` URL is returned as is.
///
/// ```
/// use polkameter::redact::endpoint;
/// assert_eq!(
///     endpoint("wss://user:pass@rpc.example:443/ws?apikey=k&v=1#t"),
///     "wss://[redacted]@rpc.example:443/ws?apikey=[redacted]&v=[redacted]#[redacted]"
/// );
/// ```
pub fn endpoint(url: &str) -> String {
	let Some(UrlParts { scheme, authority, path, query, fragment }) = split_url(url) else {
		return url.to_owned();
	};
	let mut out = String::from(scheme);
	match authority.rsplit_once('@') {
		Some((_, host)) => {
			out.push_str(REDACTED);
			out.push('@');
			out.push_str(host);
		},
		None => out.push_str(authority),
	}
	out.push_str(path);
	if let Some(query) = query {
		out.push('?');
		let parameters: Vec<String> = query
			.split('&')
			.map(|parameter| match parameter.split_once('=') {
				Some((name, "")) => format!("{name}="),
				Some((name, _)) => format!("{name}={REDACTED}"),
				// A bare value, such as `?token`, may be a key itself.
				None if parameter.is_empty() => String::new(),
				None => REDACTED.to_owned(),
			})
			.collect();
		out.push_str(&parameters.join("&"));
	}
	if let Some(fragment) = fragment {
		out.push('#');
		if !fragment.is_empty() {
			out.push_str(REDACTED);
		}
	}
	out
}

/// The secret parts of `url`, as written and percent-decoded: its user information and password,
/// every query value and its fragment.
fn endpoint_secrets(url: &str) -> Vec<String> {
	let Some(UrlParts { authority, query, fragment, .. }) = split_url(url) else {
		return Vec::new();
	};

	let mut parts = Vec::new();
	if let Some((userinfo, _)) = authority.rsplit_once('@') {
		parts.push(userinfo);
		if let Some((_, password)) = userinfo.split_once(':') {
			parts.push(password);
		}
	}
	for parameter in query.into_iter().flat_map(|query| query.split('&')) {
		parts.push(parameter.split_once('=').map_or(parameter, |(_, value)| value));
	}
	parts.extend(fragment);
	let mut out = Vec::new();
	for part in parts.into_iter().filter(|part| !part.is_empty()) {
		out.push(part.to_owned());
		let decoded = percent_decode(part);
		if decoded != part {
			out.push(decoded);
		}
	}
	out
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
#[derive(Debug)]
pub struct StreamRedactor {
	redactor: Redactor,
	/// The last released bytes, raw: at most the longest secret's length minus one.
	context: Vec<u8>,
	pending: Vec<u8>,
	limit: usize,
}

impl StreamRedactor {
	/// A stream redactor that holds partial lines up to [`STREAM_LINE_LIMIT`] bytes.
	pub fn new(redactor: Redactor) -> Self {
		Self::with_limit(redactor, STREAM_LINE_LIMIT)
	}

	/// A stream redactor that holds partial lines up to `limit` bytes.
	pub fn with_limit(redactor: Redactor, limit: usize) -> Self {
		Self { redactor, context: Vec::new(), pending: Vec::new(), limit }
	}

	/// Takes the next chunk and returns the redacted bytes that can be written now.
	pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
		if self.redactor.is_empty() {
			return chunk.to_vec();
		}
		self.pending.extend_from_slice(chunk);
		let hay = [self.context.as_slice(), &self.pending].concat();
		let safe = self.redactor.partial_start(&hay).saturating_sub(self.context.len());
		let over_limit = self.pending.len() > self.limit;
		let mut cut = if over_limit {
			safe
		} else {
			let line_end = self.pending.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
			line_end.min(safe)
		};
		// Do not cut through a secret that is already complete: release it whole, or, when the
		// line can wait, keep it whole for the next release.
		let from = self.context.len();
		if let Some(group) = self
			.redactor
			.groups(&hay)
			.into_iter()
			.find(|group| group.start < from + cut && from + cut < group.end)
		{
			cut = if !over_limit && group.start > from {
				group.start - from
			} else {
				group.end - from
			};
		}
		self.release(&hay, cut)
	}

	/// Returns the rest of the stream, redacted, once it has ended.
	pub fn finish(&mut self) -> Vec<u8> {
		let hay = [self.context.as_slice(), &self.pending].concat();
		let cut = self.pending.len();
		self.release(&hay, cut)
	}

	/// Releases `pending[..cut]`, where `hay` is the context followed by the pending bytes.
	fn release(&mut self, hay: &[u8], cut: usize) -> Vec<u8> {
		let mut out = Vec::new();
		if cut == 0 {
			return out;
		}
		let from = self.context.len();
		self.redactor.render(hay, from, from + cut, &mut out);
		let keep = self.redactor.longest().saturating_sub(1);
		let released = &hay[..from + cut];
		self.context = released[released.len().saturating_sub(keep)..].to_vec();
		self.pending.drain(..cut);
		out
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

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
	}

	#[test]
	fn escaped_forms_are_redacted() {
		let secret = "pa\"ss\\word\nwith é and 🔑";
		let redactor = secrets(&[secret]);
		let json = serde_json::to_string(&json!({ "key": secret })).unwrap();
		assert_eq!(redactor.text(&json), r#"{"key":"[redacted]"}"#);
		let python = r#"{"key": "pa\"ss\\word\nwith é and 🔑"}"#;
		assert_eq!(redactor.text(python), r#"{"key": "[redacted]"}"#);
		assert_eq!(redactor.text(&format!("{secret:?}")), "\"[redacted]\"");
		assert_eq!(redactor.text(&format!("raw {secret} end")), "raw [redacted] end");
	}

	#[test]
	fn values_keys_and_debug_are_redacted() {
		let redactor = secrets(&["s3cret"]);
		let mut value = json!({"s3cret": ["a s3cret", 1, {"k": "s3cret"}]});
		redactor.value(&mut value);
		assert_eq!(value, json!({"[redacted]": ["a [redacted]", 1, {"k": "[redacted]"}]}));
		assert!(!format!("{redactor:?}").contains("s3cret"));
		assert_eq!(Redactor::default().text("s3cret"), "s3cret");
		assert!(secrets(&[""]).is_empty());
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
		// Long parts are secrets on their own, also percent-decoded; short ones are not.
		assert_eq!(
			redactor.text("key K3y/Value12, pass hunter2hunter2, v 1"),
			"key [redacted], pass [redacted], v 1"
		);
		// The `url` crate's normal form, with a `/` path, is recognized too.
		let short = Redactor::new(None::<&str>, ["wss://host?k=v"]);
		assert_eq!(short.text("error at wss://host/?k=v"), "error at wss://host/?k=[redacted]");
		// A URL without secrets adds nothing.
		assert!(Redactor::new(None::<&str>, ["wss://host:443/path"]).is_empty());
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

	/// Feeds `chunks` to a stream redactor and returns everything it wrote.
	fn stream(redactor: &Redactor, limit: usize, chunks: &[&[u8]]) -> (Vec<u8>, Vec<usize>) {
		let mut stream = StreamRedactor::with_limit(redactor.clone(), limit);
		let mut out = Vec::new();
		let mut held = Vec::new();
		for chunk in chunks {
			out.extend(stream.push(chunk));
			held.push(stream.pending.len());
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
	fn over_long_lines_hold_back_at_most_the_longest_secret_minus_one() {
		let secret = "0123456789-secret";
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
