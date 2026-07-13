use rand::{rngs::StdRng, RngExt, SeedableRng};
use rand_distr::{Distribution, Exp};

use crate::scenario::ArrivalModel;

pub fn offsets(users: u32, arrival: &ArrivalModel, seed: u64) -> Result<Vec<u64>, String> {
	if users == 0 {
		return Err("virtual users must be at least one".into());
	}
	arrival.validate().map_err(str::to_owned)?;
	let mut rng = StdRng::seed_from_u64(seed);
	let mut offsets = match arrival {
		ArrivalModel::Burst { window_ms } => {
			(0..users).map(|_| rng.random_range(0..=*window_ms)).collect()
		},
		ArrivalModel::Ramp { duration_ms } if users == 1 => vec![0],
		ArrivalModel::Ramp { duration_ms } => (0..users)
			.map(|index| u64::from(index) * duration_ms / u64::from(users - 1))
			.collect(),
		ArrivalModel::Poisson { rate_per_second } => {
			let distribution = Exp::new(*rate_per_second).map_err(|error| error.to_string())?;
			let mut elapsed_seconds = 0.0;
			(0..users)
				.map(|_| {
					elapsed_seconds += distribution.sample(&mut rng);
					(elapsed_seconds * 1_000.0).round() as u64
				})
				.collect()
		},
	};
	offsets.sort_unstable();
	Ok(offsets)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn burst_is_seeded_and_kept_inside_the_window() {
		let arrival = ArrivalModel::Burst { window_ms: 1_000 };
		let one = offsets(100, &arrival, 9).expect("valid schedule");
		let two = offsets(100, &arrival, 9).expect("valid schedule");
		assert_eq!(one, two);
		assert!(one.iter().all(|offset| *offset <= 1_000));
	}

	#[test]
	fn ramp_reaches_its_end() {
		let offsets =
			offsets(4, &ArrivalModel::Ramp { duration_ms: 900 }, 1).expect("valid schedule");
		assert_eq!(offsets, vec![0, 300, 600, 900]);
	}

	#[test]
	fn poisson_is_monotonic() {
		let offsets = offsets(24, &ArrivalModel::Poisson { rate_per_second: 20.0 }, 2)
			.expect("valid schedule");
		assert!(offsets.windows(2).all(|pair| pair[0] <= pair[1]));
	}
}
