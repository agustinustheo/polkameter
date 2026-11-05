//! Numbers as the TS tool prints them, so a detail line or a summary table reads the same from
//! both tools. JavaScript rounds an exact tie away from zero (`(2.5).toFixed(0)` is `"3"`),
//! Rust's `{:.0}` to even (`"2"`); and JavaScript prints `9` where Rust's `{}` prints `9` too,
//! but `Infinity` where Rust prints `inf`.

/// JavaScript's `Number.prototype.toFixed`: the exact value rounded to `digits` places, ties
/// away from zero.
pub fn to_fixed(x: f64, digits: usize) -> String {
	if !x.is_finite() {
		return num(x);
	}
	// Rust prints the exact decimal expansion of a double when asked for enough places, so a
	// tie shows as a 5 followed by zeros only (the expansion of a tie ends right there).
	let exact = format!("{:.*}", digits + 60, x.abs());
	let (int, frac) = exact.split_once('.').expect("a fraction was asked for");
	let tie = frac.as_bytes()[digits] == b'5' && frac[digits + 1..].bytes().all(|b| b == b'0');
	if !tie {
		return format!("{x:.digits$}");
	}
	let mut kept: Vec<u8> = int.bytes().chain(frac[..digits].bytes()).collect();
	// One up in the last place, with carry.
	let mut i = kept.len();
	loop {
		if i == 0 {
			kept.insert(0, b'1');
			break;
		}
		i -= 1;
		if kept[i] == b'9' {
			kept[i] = b'0';
		} else {
			kept[i] += 1;
			break;
		}
	}
	let s = String::from_utf8(kept).expect("digits");
	let (int, frac) = s.split_at(s.len() - digits);
	let sign = if x < 0.0 { "-" } else { "" };
	if digits == 0 { format!("{sign}{int}") } else { format!("{sign}{int}.{frac}") }
}

/// JavaScript's `String(n)` for a number in a detail line: `9`, `2.5`, `Infinity`.
pub fn num(n: f64) -> String {
	if n.is_nan() {
		return "NaN".into();
	}
	if n.is_infinite() {
		return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
	}
	if n == 0.0 {
		return "0".into();
	}
	if n.abs() >= 1e21 || n.abs() < 1e-6 {
		let exp_form = format!("{n:e}");
		let (mant, exp) = exp_form.split_once('e').expect("{:e} has an exponent");
		return if exp.starts_with('-') {
			format!("{mant}e{exp}")
		} else {
			format!("{mant}e+{exp}")
		};
	}
	format!("{n}")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ties_round_away_from_zero_as_in_js() {
		assert_eq!(to_fixed(2.5, 0), "3");
		assert_eq!(to_fixed(12.5, 0), "13");
		assert_eq!(to_fixed(0.25, 1), "0.3");
		assert_eq!(to_fixed(9.95, 1), "9.9", "9.95 is below the tie in binary, as in JS");
		assert_eq!(to_fixed(1.005, 2), "1.00");
		assert_eq!(to_fixed(99.5, 0), "100");
		assert_eq!(to_fixed(-2.5, 0), "-3");
		assert_eq!(to_fixed(6.0, 1), "6.0");
		assert_eq!(to_fixed(0.0, 2), "0.00");
		assert_eq!(to_fixed(70.7, 1), "70.7");
		assert_eq!(to_fixed(f64::INFINITY, 1), "Infinity");
	}

	#[test]
	fn numbers_print_as_in_js() {
		assert_eq!(num(9.0), "9");
		assert_eq!(num(2.5), "2.5");
		assert_eq!(num(f64::INFINITY), "Infinity");
		assert_eq!(num(1e-7), "1e-7");
		assert_eq!(num(1.5e21), "1.5e+21");
	}
}
