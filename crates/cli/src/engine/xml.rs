//! A small XML reader for plan files: names and attribute values borrow from the source, elements
//! carry byte offsets, and only the predefined entities and character references are decoded.
//! DOCTYPE, processing instructions and CDATA are rejected, and nesting is capped. Characters
//! outside the XML 1.0 `Char` production, `--` inside comments and malformed XML declarations
//! are rejected with their line and column.
use anyhow::Result;
use std::{borrow::Cow, collections::BTreeSet, fmt::Display, rc::Rc};

/// Deepest allowed element nesting; plans use about five levels.
const MAX_DEPTH: usize = 16;

/// A position in a plan: 1-based line and column. Columns count characters, not bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Location {
	pub line: usize,
	pub column: usize,
}

impl Location {
	/// The position of byte `offset` in `src`. Builds a [`LineIndex`] for one lookup; use the
	/// index directly for many.
	pub fn of(src: &str, offset: usize) -> Self {
		LineIndex::new(src).locate(src, offset)
	}
}

/// Byte distance between the character-count checkpoints of a [`LineIndex`].
const CHECKPOINT_BYTES: usize = 4096;

/// Line starts and character counts of one text, built once per parse, so that each position is
/// found by a binary search plus a scan of at most [`CHECKPOINT_BYTES`] bytes, however long the
/// line. Before, every position rescanned the text from its start, so a plan with many elements
/// took quadratic time.
#[derive(Debug)]
pub struct LineIndex {
	/// Byte offset of the start of each line; the first is 0.
	starts: Vec<usize>,
	/// The number of characters before byte `i * CHECKPOINT_BYTES`.
	checkpoints: Vec<usize>,
}

impl LineIndex {
	pub fn new(src: &str) -> Self {
		let bytes = src.as_bytes();
		let mut starts = vec![0];
		starts.extend(memchr_newlines(bytes).map(|i| i + 1));
		let mut checkpoints = Vec::with_capacity(bytes.len() / CHECKPOINT_BYTES + 1);
		let mut count = 0;
		for chunk in bytes.chunks(CHECKPOINT_BYTES) {
			checkpoints.push(count);
			count += chars_in(chunk);
		}
		checkpoints.push(count);
		Self { starts, checkpoints }
	}

	/// The number of characters before byte `offset` of `src`.
	fn chars_before(&self, src: &[u8], offset: usize) -> usize {
		let block = offset / CHECKPOINT_BYTES;
		self.checkpoints[block] + chars_in(&src[block * CHECKPOINT_BYTES..offset])
	}

	/// The line and column of byte `offset` in `src`, the text this index was built from.
	/// Columns count characters, not bytes.
	pub fn locate(&self, src: &str, offset: usize) -> Location {
		let src = src.as_bytes();
		let offset = offset.min(src.len());
		// The last line start at or before `offset`.
		let line = self.starts.partition_point(|&start| start <= offset);
		let start = self.starts[line - 1];
		let column = self.chars_before(src, offset) - self.chars_before(src, start) + 1;
		Location { line, column }
	}
}

/// Positions of `\n` in `bytes`.
fn memchr_newlines(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
	bytes.iter().enumerate().filter(|&(_, &b)| b == b'\n').map(|(i, _)| i)
}

/// Characters in `bytes`: every byte that is not a UTF-8 continuation byte starts one.
fn chars_in(bytes: &[u8]) -> usize {
	bytes.iter().filter(|&&b| (b & 0xC0) != 0x80).count()
}

impl Display for Location {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "line {}, column {}", self.line, self.column)
	}
}

/// An error in a plan, with the position it applies to when there is one. It displays as
/// `line L, column C: message`, or as the message alone. A caller that knows the plan's file
/// can downcast an [`anyhow::Error`] to this type to report the file, line and column.
#[derive(Debug)]
pub struct PlanError {
	pub location: Option<Location>,
	pub message: String,
}

impl PlanError {
	/// An error at `location`.
	pub fn at(location: Location, message: impl Display) -> anyhow::Error {
		anyhow::Error::new(Self { location: Some(location), message: message.to_string() })
	}
}

impl Display for PlanError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self.location {
			Some(location) => write!(f, "{location}: {}", self.message),
			None => f.write_str(&self.message),
		}
	}
}

impl std::error::Error for PlanError {}

/// An element. Text content is not kept: plans only use element content for children.
#[derive(Debug)]
pub struct Element<'a> {
	pub src: &'a str,
	/// The line index of `src`, shared by every element of one parse.
	pub lines: Rc<LineIndex>,
	pub name: &'a str,
	/// Byte offset of the `<`.
	pub offset: usize,
	/// Name, decoded value and byte offset of each attribute, in document order.
	pub attrs: Vec<(&'a str, Cow<'a, str>, usize)>,
	pub children: Vec<Element<'a>>,
}

impl Element<'_> {
	/// Where this element starts.
	pub fn location(&self) -> Location {
		self.locate(self.offset)
	}
	/// Where byte `offset` of the plan is.
	pub fn locate(&self, offset: usize) -> Location {
		self.lines.locate(self.src, offset)
	}
	/// An error at byte `offset` of the plan.
	pub fn error_at(&self, offset: usize, msg: impl Display) -> anyhow::Error {
		PlanError::at(self.locate(offset), msg)
	}
}

/// The message for a declaration that does not open with `version`.
const DECLARATION_VERSION: &str = "the XML declaration must start with version";

/// Parses an optional declaration, misc (whitespace and comments) and exactly one root element.
pub fn parse(src: &str) -> Result<Element<'_>> {
	// A byte-order mark is not content; offsets and columns count from the text after it.
	let src = src.strip_prefix('\u{feff}').unwrap_or(src);
	if let Some((at, c)) = src.char_indices().find(|&(_, c)| !xml_char(c)) {
		let code = u32::from(c);
		let msg = format!("character U+{code:04X} is not allowed in XML; remove it from the plan");
		return Err(error_at(src, at, msg));
	}
	let mut p = Parser { src, pos: 0, lines: Rc::new(LineIndex::new(src)) };
	if declaration_start(src) {
		p.declaration()?;
	}
	p.skip_misc()?;
	if !p.rest().starts_with('<') {
		return Err(p.error(p.pos, "expected a root element"));
	}
	let root = p.element(1)?;
	p.skip_misc()?;
	if p.pos < src.len() {
		return Err(p.error(p.pos, "unexpected content after the root element"));
	}
	Ok(root)
}

/// Whether `text` starts with an XML declaration: `<?xml` followed by whitespace or `?`, unlike a
/// processing instruction whose name only starts with `xml`.
fn declaration_start(text: &str) -> bool {
	text.strip_prefix("<?xml")
		.is_some_and(|rest| rest.starts_with([' ', '\t', '\r', '\n', '?']))
}

/// Why `encoding` in the XML declaration cannot be read, or `None` when it can. Plans are read
/// as UTF-8; an ASCII or Latin-1 declaration is accepted while the text is pure ASCII, which reads
/// the same in all three.
fn encoding_hint(encoding: &str, src: &str) -> Option<&'static str> {
	const ASCII_COMPATIBLE: [&str; 5] = ["US-ASCII", "ASCII", "ISO-8859-1", "ISO_8859-1", "LATIN1"];
	if encoding.eq_ignore_ascii_case("UTF-8") {
		return None;
	}
	if ASCII_COMPATIBLE.iter().any(|name| encoding.eq_ignore_ascii_case(name)) {
		return (!src.is_ascii()).then_some(
			"the plan has non-ASCII text, which PolkaMeter reads as UTF-8; write \
			 encoding=\"UTF-8\" (the file already is UTF-8), or remove encoding",
		);
	}
	Some(
		"PolkaMeter reads plans as UTF-8; save the file as UTF-8 and write \
		 encoding=\"UTF-8\", or remove encoding",
	)
}

/// The XML 1.0 `Char` production. A Rust string cannot hold a surrogate, so none is checked.
fn xml_char(c: char) -> bool {
	matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

/// An error at a byte offset, formatted as `line L, column C: message`.
pub fn error_at(src: &str, offset: usize, msg: impl Display) -> anyhow::Error {
	PlanError::at(Location::of(src, offset), msg)
}

struct Parser<'a> {
	src: &'a str,
	pos: usize,
	lines: Rc<LineIndex>,
}

impl<'a> Parser<'a> {
	fn rest(&self) -> &'a str {
		&self.src[self.pos..]
	}

	fn error(&self, offset: usize, msg: impl Display) -> anyhow::Error {
		PlanError::at(self.lines.locate(self.src, offset), msg)
	}

	/// Skips XML whitespace; true if there was any.
	fn skip_ws(&mut self) -> bool {
		let rest = self.rest();
		let n = rest.len() - rest.trim_start_matches([' ', '\t', '\r', '\n']).len();
		self.pos += n;
		n > 0
	}

	/// Parses the XML declaration at the start of the document:
	/// `<?xml version="1.x" encoding="UTF-8" standalone="yes|no"?>`, where encoding and standalone
	/// are optional but keep this order.
	fn declaration(&mut self) -> Result<()> {
		let start = self.pos;
		let end = start
			+ self
				.rest()
				.find("?>")
				.ok_or_else(|| self.error(start, "unterminated XML declaration"))?;
		self.pos += "<?xml".len();
		let mut allowed: &[&str] = &["version", "encoding", "standalone"];
		loop {
			let spaced = self.skip_ws();
			if self.pos >= end {
				break;
			}
			let at = self.pos;
			let first = allowed.len() == 3;
			let len =
				self.src[at..end].find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(end - at);
			let name = &self.src[at..at + len];
			let index = allowed.iter().position(|known| *known == name);
			let Some(index) = index.filter(|&index| !first || index == 0) else {
				if first {
					return Err(self.error(start, DECLARATION_VERSION));
				}
				let msg = "the XML declaration allows only version, encoding and standalone, in \
				           that order";
				return Err(self.error(at, msg));
			};
			if !spaced {
				let msg = format!("expected whitespace before `{name}` in the XML declaration");
				return Err(self.error(at, msg));
			}
			allowed = &allowed[index + 1..];
			self.pos = at + len;
			self.skip_ws();
			if !self.rest().starts_with('=') {
				if first {
					return Err(self.error(start, DECLARATION_VERSION));
				}
				let msg = format!("expected `=` after `{name}` in the XML declaration");
				return Err(self.error(self.pos, msg));
			}
			self.pos += 1;
			self.skip_ws();
			let Some(quote) = self.rest().chars().next().filter(|q| matches!(q, '"' | '\'')) else {
				let msg = format!("expected a quoted value for `{name}` in the XML declaration");
				return Err(self.error(self.pos, msg));
			};
			let value_at = self.pos + 1;
			let len = self.src[value_at..end].find(quote).ok_or_else(|| {
				let msg = format!("unterminated `{name}` value in the XML declaration");
				self.error(value_at, msg)
			})?;
			let value = &self.src[value_at..value_at + len];
			let hint = match name {
				"version" => value
					.strip_prefix("1.")
					.filter(|minor| !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit()))
					.map_or(Some("use version=\"1.0\""), |_| None),
				"encoding" => encoding_hint(value, self.src),
				_ => (!matches!(value, "yes" | "no"))
					.then_some("use standalone=\"yes\" or standalone=\"no\""),
			};
			if let Some(hint) = hint {
				let msg = format!("invalid {name} `{value}` in the XML declaration; {hint}");
				return Err(self.error(value_at, msg));
			}
			self.pos = value_at + len + 1;
		}
		if allowed.len() == 3 {
			return Err(self.error(start, DECLARATION_VERSION));
		}
		self.pos = end + 2;
		Ok(())
	}

	/// Skips whitespace and comments. Any other markup here (DOCTYPE, processing instruction,
	/// CDATA or declaration) is rejected.
	fn skip_misc(&mut self) -> Result<()> {
		self.skip_ws();
		while self.rest().starts_with("<!--") {
			let start = self.pos;
			self.pos += "<!--".len();
			let dashes = self
				.rest()
				.find("--")
				.ok_or_else(|| self.error(start, "unterminated comment"))?;
			self.pos += dashes;
			if !self.rest().starts_with("-->") {
				let msg =
					"`--` is not allowed inside a comment, and a comment cannot end with `--->`";
				return Err(self.error(self.pos, msg));
			}
			self.pos += "-->".len();
			self.skip_ws();
		}
		let rest = self.rest();
		let msg = if declaration_start(rest) {
			"the XML declaration must come first in the file, before any whitespace or comment"
		} else if rest.starts_with("<!DOCTYPE") {
			"DOCTYPE is not supported"
		} else if rest.starts_with("<?") {
			"processing instructions are not supported"
		} else if rest.starts_with("<!") {
			"CDATA and declarations are not supported"
		} else {
			return Ok(());
		};
		Err(self.error(self.pos, msg))
	}

	fn name(&mut self) -> Result<&'a str> {
		let rest = self.rest();
		let bytes = rest.as_bytes();
		if !bytes.first().is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_') {
			return Err(self.error(self.pos, "expected a name"));
		}
		let len = bytes
			.iter()
			.take_while(|&&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
			.count();
		self.pos += len;
		Ok(&rest[..len])
	}

	/// Parses the element starting at `<`; `depth` counts the root as 1.
	fn element(&mut self, depth: usize) -> Result<Element<'a>> {
		let offset = self.pos;
		if depth > MAX_DEPTH {
			return Err(self.error(offset, "nesting deeper than 16 levels"));
		}
		self.pos += 1;
		let name = self.name()?;
		let mut attrs: Vec<(&'a str, Cow<'a, str>, usize)> = Vec::new();
		let mut seen = BTreeSet::new();
		loop {
			let spaced = self.skip_ws();
			if self.rest().starts_with("/>") {
				self.pos += 2;
				return Ok(Element {
					src: self.src,
					lines: Rc::clone(&self.lines),
					name,
					offset,
					attrs,
					children: Vec::new(),
				});
			}
			if self.rest().starts_with('>') {
				self.pos += 1;
				break;
			}
			if !spaced {
				return Err(self.error(self.pos, "expected whitespace before an attribute"));
			}
			let at = self.pos;
			let key = self.name()?;
			self.skip_ws();
			if !self.rest().starts_with('=') {
				return Err(self.error(self.pos, "expected `=` after attribute name"));
			}
			self.pos += 1;
			self.skip_ws();
			let quote = self.rest().chars().next().filter(|q| matches!(q, '"' | '\''));
			let Some(quote) = quote else {
				return Err(self.error(self.pos, "expected a quoted attribute value"));
			};
			self.pos += 1;
			let start = self.pos;
			let len = self
				.rest()
				.find(quote)
				.ok_or_else(|| self.error(start, "unterminated attribute value"))?;
			let raw = &self.rest()[..len];
			if let Some(lt) = raw.find('<') {
				return Err(self.error(start + lt, "`<` is not allowed in attribute values"));
			}
			let value = self.decode(raw, start)?;
			self.pos = start + len + 1;
			if !seen.insert(key) {
				return Err(self.error(at, format!("duplicate attribute `{key}`")));
			}
			attrs.push((key, value, at));
		}
		let mut children = Vec::new();
		loop {
			self.skip_misc()?;
			let rest = self.rest();
			if rest.is_empty() {
				return Err(self.error(offset, format!("element <{name}> is not closed")));
			}
			if rest.starts_with("</") {
				self.pos += 2;
				let at = self.pos;
				let end = self.name()?;
				if end != name {
					let opened = self.lines.locate(self.src, offset);
					let msg = format!(
						"expected </{name}>, found </{end}>; <{name}> was opened at {opened}"
					);
					return Err(self.error(at, msg));
				}
				self.skip_ws();
				if !self.rest().starts_with('>') {
					return Err(self.error(self.pos, "expected `>` after end tag"));
				}
				self.pos += 1;
				return Ok(Element {
					src: self.src,
					lines: Rc::clone(&self.lines),
					name,
					offset,
					attrs,
					children,
				});
			}
			if !rest.starts_with('<') {
				return Err(self.error(self.pos, "text is not allowed in element content"));
			}
			children.push(self.element(depth + 1)?);
		}
	}

	/// Decodes an attribute value whose text starts at byte `base`: literal whitespace becomes a
	/// space first, then references are decoded, so `&#xA;` still yields a newline.
	fn decode(&self, raw: &'a str, base: usize) -> Result<Cow<'a, str>> {
		if !raw.contains(['&', '\t', '\n', '\r']) {
			return Ok(Cow::Borrowed(raw));
		}
		let mut out = String::with_capacity(raw.len());
		let mut i = 0;
		while let Some(found) = raw[i..].find('&') {
			let at = i + found;
			out.push_str(&raw[i..at].replace("\r\n", " ").replace(['\t', '\n', '\r'], " "));
			let end = raw[at..].find(';').ok_or_else(|| {
				let msg = "unterminated entity reference: `&` starts a reference such as \
					           `&lt;`; write a literal `&` as `&amp;`";
				self.error(base + at, msg)
			})?;
			let name = &raw[at + 1..at + end];
			let decoded = entity(name).ok_or_else(|| {
				let what = if name.starts_with('#') { "character reference" } else { "entity" };
				self.error(
					base + at,
					format!(
						"invalid {what} `&{name};`; use `&amp;`, `&lt;`, `&gt;`, `&quot;`, \
						 `&apos;` or a character reference such as `&#65;`"
					),
				)
			})?;
			out.push(decoded);
			i = at + end + 1;
		}
		out.push_str(&raw[i..].replace("\r\n", " ").replace(['\t', '\n', '\r'], " "));
		Ok(Cow::Owned(out))
	}
}

/// The predefined entities and decimal or hexadecimal (`#x`) character references, which must
/// name a character allowed by the XML `Char` production.
fn entity(name: &str) -> Option<char> {
	match name {
		"amp" => Some('&'),
		"lt" => Some('<'),
		"gt" => Some('>'),
		"quot" => Some('"'),
		"apos" => Some('\''),
		_ => {
			let (digits, radix) = match name.strip_prefix("#x") {
				Some(hex) => (hex, 16),
				None => (name.strip_prefix('#')?, 10),
			};
			let valid = !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix));
			let code = u32::from_str_radix(digits, radix).ok().filter(|_| valid)?;
			char::from_u32(code).filter(|&c| xml_char(c))
		},
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn accepts_declaration_comments_and_references() {
		let xml = "<?xml version=\"1.0\"?>\n<!-- top --><a x='&lt;&#65;&#x42;' y=\"plain\">\n<!-- in --><b/>\n</a>";
		let root = parse(xml).unwrap();
		assert_eq!((root.name, root.children[0].name), ("a", "b"));
		assert_eq!(root.attrs[0].1, "<AB");
		assert!(matches!(root.attrs[1].1, Cow::Borrowed("plain")));
	}

	#[test]
	fn rejects_malformed_documents() {
		let cases = [
			(r#"<a x="1" x="2"/>"#, "duplicate attribute `x`"),
			("<!DOCTYPE a><a/>", "DOCTYPE is not supported"),
			("<?pi x?><a/>", "processing instructions are not supported"),
			("<?xml?><a/>", "the XML declaration must start with version"),
			(r#"<?xml encoding="UTF-8"?><a/>"#, "the XML declaration must start with version"),
			(r#"<?xml versionfoo="1"?><a/>"#, "the XML declaration must start with version"),
			(r#"<a x="1"y="2"/>"#, "expected whitespace before an attribute"),
			("<a><![CDATA[x]]></a>", "CDATA and declarations are not supported"),
			("<a>text</a>", "text is not allowed"),
			("<a/><b/>", "unexpected content after the root element"),
			(r#"<a x="<"/>"#, "`<` is not allowed in attribute values"),
			(r#"<a x="&nope;"/>"#, "invalid entity `&nope;`"),
			(r#"<a x="&#0;"/>"#, "invalid character reference `&#0;`"),
			(r#"<a x="&#1;"/>"#, "invalid character reference `&#1;`"),
			(r#"<a x="&#xB;"/>"#, "invalid character reference `&#xB;`"),
			(r#"<a x="&#xFFFE;"/>"#, "invalid character reference `&#xFFFE;`"),
			("<a>\n<b/>\n<c></d>\n</a>", "line 3, column 6: expected </c>, found </d>"),
			("\u{feff}<a>\n<c></d>\n</a>", "line 2, column 6: expected </c>, found </d>"),
			(&"<a>".repeat(17), "nesting deeper than 16 levels"),
		];
		for (xml, msg) in cases {
			let error = parse(xml).unwrap_err().to_string();
			assert!(error.contains(msg), "{xml}: {error}");
		}
	}

	#[test]
	fn rejects_malformed_xml_declarations() {
		let cases = [
			(r#"<?xml version="2.0"?><a/>"#, "line 1, column 16: invalid version `2.0`"),
			(r#"<?xml version="1."?><a/>"#, "invalid version `1.`"),
			(
				r#"<?xml version="1.0"encoding="UTF-8"?><a/>"#,
				"expected whitespace before `encoding`",
			),
			(r#"<?xml version="1.0" encoding="UTF-16"?><a/>"#, "invalid encoding `UTF-16`"),
			(r#"<?xml version="1.0" standalone="maybe"?><a/>"#, "invalid standalone `maybe`"),
			(
				r#"<?xml version="1.0" standalone="yes" encoding="UTF-8"?><a/>"#,
				"allows only version",
			),
			(
				r#"<?xml version="1.0" junk="1"?><a/>"#,
				"line 1, column 21: the XML declaration allows only",
			),
			(
				r#"<?xml version="1.0" version="1.0"?><a/>"#,
				"allows only version, encoding and standalone",
			),
			(r#"<?xml version=1.0?><a/>"#, "expected a quoted value for `version`"),
			(r#"<?xml version="1.0?><a/>"#, "unterminated `version` value"),
			(r#"<?xml version="1.0" encoding?><a/>"#, "expected `=` after `encoding`"),
			(r#"<?xml version="1.0" ?"#, "unterminated XML declaration"),
			(
				"\n<?xml version=\"1.0\"?><a/>",
				"line 2, column 1: the XML declaration must come first",
			),
			("<!-- c --><?xml version=\"1.0\"?><a/>", "the XML declaration must come first"),
		];
		for (xml, msg) in cases {
			let error = parse(xml).unwrap_err().to_string();
			assert!(error.contains(msg), "{xml}: {error}");
		}
		for xml in [
			r#"<?xml version="1.0"?><a/>"#,
			r#"<?xml version='1.1' encoding='utf-8' standalone='no' ?><a/>"#,
			"<?xml\tversion = \"1.0\"\nencoding=\"UTF-8\"?>\n<a/>",
		] {
			parse(xml).unwrap_or_else(|e| panic!("{xml}: {e}"));
		}
	}

	#[test]
	fn rejects_double_hyphens_in_comments() {
		for (xml, column) in [
			("<!-- a -- b --><a/>", 8),
			("<!-- a ---><a/>", 8),
			("<a>\n  <!-- x--y --></a>", 9),
			("<a/><!-- -- -->", 10),
		] {
			let error = parse(xml).unwrap_err().to_string();
			assert!(
				error.contains(&format!("column {column}: `--` is not allowed")),
				"{xml}: {error}"
			);
		}
		parse("<!----><a><!-- a - b --></a><!-- - -->").unwrap();
		let error = parse("<a/><!-- open").unwrap_err().to_string();
		assert!(error.contains("line 1, column 5: unterminated comment"), "{error}");
	}

	#[test]
	fn rejects_characters_outside_xml() {
		for (xml, shown) in [
			("<a x=\"\0\"/>", "line 1, column 7: character U+0000"),
			("<a>\n<b y=\"\u{1}\"/></a>", "line 2, column 7: character U+0001"),
			("<a/><!-- \u{1b} -->", "character U+001B"),
			("<a x=\"\u{FFFE}\"/>", "character U+FFFE"),
			("<a x=\"\u{FFFF}\"/>", "character U+FFFF"),
			("<a/>\u{0}", "character U+0000"),
		] {
			let error = parse(xml).unwrap_err().to_string();
			assert!(error.contains(shown), "{xml:?}: {error}");
			assert!(error.contains("is not allowed in XML"), "{error}");
		}
		parse("<a x=\"\t\u{7F}\u{85}\u{E000}\u{10FFFF}\"/>").unwrap();
	}

	#[test]
	fn errors_carry_their_location() {
		let error = parse("<a>\n  <b></c>\n</a>").unwrap_err();
		let located = error.downcast_ref::<PlanError>().unwrap();
		assert_eq!(located.location, Some(Location { line: 2, column: 8 }));
		assert_eq!(
			located.message,
			"expected </b>, found </c>; <b> was opened at line 2, column 3"
		);
	}

	#[test]
	fn line_index_matches_a_full_rescan() {
		/// The position of byte `offset`, counted from the start as the parser once did.
		fn rescan(src: &str, offset: usize) -> Location {
			let before = &src.as_bytes()[..offset];
			let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
			let start = before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
			let column = before[start..].iter().filter(|&&b| (b & 0xC0) != 0x80).count() + 1;
			Location { line, column }
		}
		// Multi-byte characters straddle checkpoints, and one line spans several of them.
		let mut src = String::new();
		for i in 0..3_000 {
			src += if i % 7 == 0 { "\n" } else { "é界🚀ab" };
			if i % 500 == 0 {
				src += &"x".repeat(CHECKPOINT_BYTES + 3);
			}
		}
		let index = LineIndex::new(&src);
		for (offset, _) in src.char_indices().step_by(13).chain([(src.len(), ' ')]) {
			assert_eq!(index.locate(&src, offset), rescan(&src, offset), "offset {offset}");
		}
		assert_eq!(index.locate(&src, usize::MAX), rescan(&src, src.len()));
	}

	#[test]
	fn many_elements_parse_in_linear_time() {
		// About 2 MiB with tens of thousands of elements, each located once. A rescan per
		// element took minutes for this; the index takes milliseconds.
		let step = "<s id=\"a\" k=\"v\"/>\n";
		let count = 2 * 1024 * 1024 / step.len() - 1;
		let xml = format!("<a>\n{}</a>", step.repeat(count));
		let started = std::time::Instant::now();
		let root = parse(&xml).unwrap();
		let locations: Vec<_> = root.children.iter().map(Element::location).collect();
		let last = *locations.last().unwrap();
		let elapsed = started.elapsed();
		assert_eq!(last, Location { line: count + 1, column: 1 });
		// Generous, and scaled for an unoptimised build.
		let limit = if cfg!(debug_assertions) { 20 } else { 2 };
		assert!(elapsed < std::time::Duration::from_secs(limit), "took {elapsed:?}");
	}

	#[test]
	fn messages_say_how_to_fix_references_and_encodings() {
		let cases = [
			(r#"<a x="fish & chips"/>"#, "unterminated entity reference: `&` starts a reference"),
			(r#"<a x="fish & chips"/>"#, "write a literal `&` as `&amp;`"),
			(r#"<a x="&nope;"/>"#, "use `&amp;`, `&lt;`"),
			(
				"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a x=\"café\"/>",
				"the plan has non-ASCII text, which PolkaMeter reads as UTF-8",
			),
		];
		for (xml, msg) in cases {
			let error = parse(xml).unwrap_err().to_string();
			assert!(error.contains(msg), "{xml}: {error}");
		}
		for encoding in ["US-ASCII", "ascii", "ISO-8859-1", "latin1"] {
			let xml = format!("<?xml version=\"1.0\" encoding=\"{encoding}\"?><a x=\"plain\"/>");
			parse(&xml).unwrap_or_else(|e| panic!("{encoding}: {e}"));
		}
	}

	#[test]
	fn normalises_literal_whitespace_in_attribute_values() {
		let root = parse("<a x=\"a\nb\" y=\"a\r\nb\tc\" z=\"&#xA;&#xD;\"/>").unwrap();
		assert_eq!(root.attrs[0].1, "a b");
		assert_eq!(root.attrs[1].1, "a b c");
		assert_eq!(root.attrs[2].1, "\n\r");
		assert!(matches!(root.attrs[0].1, Cow::Owned(_)));
	}

	#[test]
	fn accepts_byte_order_mark_and_tab_references() {
		let root = parse("\u{feff}<a x=\"&#9;&#x10000;\"/>").unwrap();
		assert_eq!(root.name, "a");
		assert_eq!(root.attrs[0].1, "\t\u{10000}");
	}

	#[test]
	fn many_attributes_are_checked_in_linear_time() {
		let mut xml = String::from("<a");
		for i in 0..200_000 {
			xml += &format!(" k{i}=\"1\"");
		}
		xml += " k5=\"2\"/>";
		let error = parse(&xml).unwrap_err().to_string();
		assert!(error.contains("duplicate attribute `k5`"), "{error}");
	}
}
