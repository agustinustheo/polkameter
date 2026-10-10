//! Reading a dynamic [`Value`] (an event's fields, a storage enum) without a type for it: a
//! field by name at any depth, a number through its newtype wrappers, a 32-byte key.

use scale_value::{Composite, Primitive, ValueDef};

use crate::Value;

fn children(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
	match &v.value {
		ValueDef::Composite(c) => composite_values(c),
		ValueDef::Variant(var) => composite_values(&var.values),
		_ => Box::new(std::iter::empty()),
	}
}

/// The only field of a single-field composite or variant (a newtype wrapper).
fn only_child(v: &Value) -> Option<&Value> {
	let mut c = children(v);
	let first = c.next()?;
	c.next().is_none().then_some(first)
}

fn composite_values(c: &Composite<()>) -> Box<dyn Iterator<Item = &Value> + '_> {
	match c {
		Composite::Named(fields) => Box::new(fields.iter().map(|(_, v)| v)),
		Composite::Unnamed(values) => Box::new(values.iter()),
	}
}

/// The first field called `name`, at any depth (depth first).
pub fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
	let fields = match &v.value {
		ValueDef::Composite(Composite::Named(f)) => Some(f),
		ValueDef::Variant(var) => match &var.values {
			Composite::Named(f) => Some(f),
			Composite::Unnamed(_) => None,
		},
		_ => None,
	};
	if let Some((_, v)) = fields.and_then(|f| f.iter().find(|(k, _)| k == name)) {
		return Some(v);
	}
	children(v).find_map(|c| field(c, name))
}

/// The `i`-th value of a composite or variant, named or not.
pub fn nth(v: &Value, i: usize) -> Option<&Value> {
	children(v).nth(i)
}

/// The variant's name, for an enum value.
pub fn variant_name(v: &Value) -> Option<&str> {
	match &v.value {
		ValueDef::Variant(var) => Some(&var.name),
		_ => None,
	}
}

/// A number, through any single-field wrappers (`ParaId(u32)`, `Compact<u64>`).
pub fn as_u64(v: &Value) -> Option<u64> {
	match &v.value {
		ValueDef::Primitive(Primitive::U128(n)) => u64::try_from(*n).ok(),
		ValueDef::Primitive(Primitive::I128(n)) => u64::try_from(*n).ok(),
		ValueDef::Primitive(_) | ValueDef::BitSequence(_) => None,
		_ => only_child(v).and_then(as_u64),
	}
}

/// 32 bytes, through any single-field wrappers.
/// Part of the plugin API (used by out-of-tree plugins).
pub fn as_bytes32(v: &Value) -> Option<[u8; 32]> {
	let bytes: Vec<u8> = children(v)
		.map(|b| as_u64(b).and_then(|n| u8::try_from(n).ok()))
		.collect::<Option<_>>()?;
	if bytes.len() == 32 {
		return bytes.try_into().ok();
	}
	as_bytes32(only_child(v)?)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_through_wrappers() {
		let receipt = Value::named_composite([
			(
				"descriptor",
				Value::named_composite([(
					"para_id",
					Value::unnamed_composite([Value::u128(1502)]),
				)]),
			),
			("commitments_hash", Value::from_bytes([1u8; 32])),
		]);
		let event = Value::unnamed_composite([receipt, Value::u128(3)]);
		let r = nth(&event, 0).unwrap();
		assert_eq!(field(r, "para_id").and_then(as_u64), Some(1502));
		assert_eq!(field(r, "commitments_hash").and_then(as_bytes32), Some([1u8; 32]));
		assert_eq!(
			as_u64(&Value::named_composite([("a", Value::u128(1)), ("b", Value::u128(2))])),
			None,
			"two fields is not a wrapper"
		);
		let pos = Value::named_variant(
			"Included",
			[
				("ring_index", Value::u128(4)),
				("ring_page", Value::u128(0)),
				("ring_position", Value::u128(9)),
			],
		);
		assert_eq!(variant_name(&pos), Some("Included"));
		assert_eq!(field(&pos, "ring_position").and_then(as_u64), Some(9));
	}
}
