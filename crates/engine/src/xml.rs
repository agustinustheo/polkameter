//! A small XML reader for plan files: names and attribute values borrow from the source, elements
//! carry byte offsets, and only the predefined entities and character references are decoded.
//! DOCTYPE, processing instructions and CDATA are rejected, and nesting is capped.
use anyhow::{Result, anyhow};
use std::{borrow::Cow, collections::BTreeSet, fmt::Display};

/// Deepest allowed element nesting; plans use about five levels.
const MAX_DEPTH: usize = 16;

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

/// Parses an optional declaration, misc (whitespace and comments) and exactly one root element.
pub fn parse(src: &str) -> Result<Element<'_>> {
	// A byte-order mark is not content; offsets and columns count from the text after it.
	let src = src.strip_prefix('\u{feff}').unwrap_or(src);
	let mut p = Parser { src, pos: 0 };
	if src.starts_with("<?xml")
		&& src[5..].starts_with(|c: char| c.is_ascii_whitespace() || c == '?')
	{
		p.pos = 5;
		p.skip_past("?>", "unterminated XML declaration")?;
		let body = src[5..p.pos - 2].trim_start_matches([' ', '\t', '\r', '\n']);
		let after = body
			.strip_prefix("version")
			.map(|r| r.trim_start_matches([' ', '\t', '\r', '\n']));
		if !after.is_some_and(|r| r.starts_with('=')) {
			return Err(p.error(0, "the XML declaration must start with version"));
		}
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

/// An error at a byte offset, formatted as `line L, column C: message`.
pub fn error_at(src: &str, offset: usize, msg: impl Display) -> anyhow::Error {
	let before = &src.as_bytes()[..offset.min(src.len())];
	let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
	let start = before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
	// Count characters, not continuation bytes, so multi-byte text does not skew the column.
	let column = before[start..].iter().filter(|&&b| (b & 0xC0) != 0x80).count() + 1;
	anyhow!("line {line}, column {column}: {msg}")
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

	/// Moves past the next `end`.
	fn skip_past(&mut self, end: &str, msg: &str) -> Result<()> {
		let found = self.rest().find(end).ok_or_else(|| self.error(self.pos, msg))?;
		self.pos += found + end.len();
		Ok(())
	}

	/// Skips whitespace and comments. Any other markup here (DOCTYPE, processing instruction,
	/// CDATA or declaration) is rejected.
	fn skip_misc(&mut self) -> Result<()> {
		self.skip_ws();
		while self.rest().starts_with("<!--") {
			self.pos += 4;
			self.skip_past("-->", "unterminated comment")?;
			self.skip_ws();
		}
		let rest = self.rest();
		let msg = if rest.starts_with("<!DOCTYPE") {
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
			char::from_u32(code).filter(|&c| {
				matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
			})
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
