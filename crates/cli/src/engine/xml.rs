//! A small XML reader for plan files: names and attribute values borrow from the source, elements
//! carry byte offsets, and only the predefined entities and character references are decoded.
//! DOCTYPE, processing instructions and CDATA are rejected, and nesting is capped. Characters
//! outside the XML 1.0 `Char` production, `--` inside comments and malformed XML declarations
//! are rejected with their line and column.
use anyhow::Result;
use std::{borrow::Cow, collections::BTreeSet, fmt::Display};

/// Deepest allowed element nesting; plans use about five levels.
const MAX_DEPTH: usize = 16;

/// A position in a plan: 1-based line and column. Columns count characters, not bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Location {
	pub line: usize,
	pub column: usize,
}

impl Location {
	/// The position of byte `offset` in `src`.
	pub fn of(src: &str, offset: usize) -> Self {
		let before = &src.as_bytes()[..offset.min(src.len())];
		let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
		let start = before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
		// Count characters, not continuation bytes, so multi-byte text does not skew the column.
		let column = before[start..].iter().filter(|&&b| (b & 0xC0) != 0x80).count() + 1;
		Self { line, column }
	}
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
		Location::of(self.src, self.offset)
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
		return Err(error_at(
			src,
			at,
			format!("character U+{code:04X} is not allowed in XML; remove it from the plan"),
		));
	}
	let mut p = Parser { src, pos: 0 };
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
}

impl<'a> Parser<'a> {
	fn rest(&self) -> &'a str {
		&self.src[self.pos..]
	}

	fn error(&self, offset: usize, msg: impl Display) -> anyhow::Error {
		error_at(self.src, offset, msg)
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
				"encoding" => (!value.eq_ignore_ascii_case("UTF-8")).then_some(
					"PolkaMeter reads plans as UTF-8; save the file as UTF-8 and write \
					 encoding=\"UTF-8\", or remove encoding",
				),
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
				return Ok(Element { src: self.src, name, offset, attrs, children: Vec::new() });
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
					return Err(self.error(at, format!("expected </{name}>, found </{end}>")));
				}
				self.skip_ws();
				if !self.rest().starts_with('>') {
					return Err(self.error(self.pos, "expected `>` after end tag"));
				}
				self.pos += 1;
				return Ok(Element { src: self.src, name, offset, attrs, children });
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
			let end = raw[at..]
				.find(';')
				.ok_or_else(|| self.error(base + at, "unterminated entity reference"))?;
			let name = &raw[at + 1..at + end];
			let decoded = entity(name).ok_or_else(|| {
				let what = if name.starts_with('#') { "character reference" } else { "entity" };
				self.error(base + at, format!("invalid {what} `&{name};`"))
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
		assert_eq!(located.message, "expected </b>, found </c>");
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
