//! Reading a dynamic [`Value`] (an event's fields, a storage enum) without a type for it: a
//! field by name at any depth, a number through its newtype wrappers, a 32-byte key.

use scale_value::{Composite, Primitive, ValueDef};

use crate::chain::Value;

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

/// A number up to `u128`, such as a balance, through any single-field wrappers.
pub fn as_u128(v: &Value) -> Option<u128> {
	match &v.value {
		ValueDef::Primitive(Primitive::U128(n)) => Some(*n),
		ValueDef::Primitive(Primitive::I128(n)) => u128::try_from(*n).ok(),
		ValueDef::Primitive(_) | ValueDef::BitSequence(_) => None,
		_ => only_child(v).and_then(as_u128),
	}
}

/// 32 bytes, through any single-field wrappers.
pub fn as_bytes32(v: &Value) -> Option<[u8; 32]> {
	let bytes: Vec<u8> = children(v)
		.map(|b| as_u64(b).and_then(|n| u8::try_from(n).ok()))
		.collect::<Option<_>>()?;
	if bytes.len() == 32 {
		return bytes.try_into().ok();
	}
	as_bytes32(only_child(v)?)
}
