//! A clock that keeps counting while the machine sleeps, to tell a suspended machine from a
//! wall-clock step. The run's own clock (`now_ms`, tokio's `Instant`) stops during a suspend;
//! this one does not, and neither moves when NTP steps the wall clock.
//!
//! `CLOCK_BOOTTIME` on Linux and Android; `CLOCK_MONOTONIC` on macOS and iOS, which there counts
//! sleep (unlike `CLOCK_UPTIME_RAW`, which `Instant` uses). Elsewhere there is none, and only a
//! long gap between send ticks shows a pause.

use polkameter_plugin_sdk::observers::files::Millis;

/// Milliseconds on a clock that counts sleep and ignores wall-clock steps; `None` on platforms
/// without one.
pub fn boot_ms() -> Option<Millis> {
	#[cfg(any(target_os = "linux", target_os = "android"))]
	let id = rustix::time::ClockId::Boottime;
	#[cfg(target_vendor = "apple")]
	let id = rustix::time::ClockId::Monotonic;
	#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
	{
		let t = rustix::time::clock_gettime(id);
		Some(t.tv_sec as Millis * 1000 + t.tv_nsec as Millis / 1_000_000)
	}
	#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
	{
		None
	}
}

#[cfg(test)]
mod tests {
	#[test]
	fn the_boot_clock_moves_forward() {
		let Some(a) = super::boot_ms() else { return };
		std::thread::sleep(std::time::Duration::from_millis(20));
		let b = super::boot_ms().unwrap();
		assert!(b >= a + 15, "{a} {b}");
	}
}
