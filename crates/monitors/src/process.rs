//! Samples the CPU and memory of the node under load with `ps` every few seconds, since the node
//! exports no process metrics on macOS. CPU is the change in the node's CPU time over wall
//! time, so 250% means 2.5 cores busy.
//!
//! The process is `POLKAMETER_NODE_PID`, or the one listening on the node's RPC port. Neither
//! found: no samples, with a warning.

use std::time::Duration;

use polkameter_files::{FileError, JsonlWriter, Millis, NodeSample, now_ms};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const EVERY: Duration = Duration::from_secs(5);

/// `ps -o time=` is `[[dd-]hh:]mm:ss[.cc]` on Linux and macOS.
fn cpu_seconds(time: &str) -> Option<f64> {
	let (days, rest) =
		time.split_once('-').map_or((0.0, time), |(d, r)| (d.parse().unwrap_or(0.0), r));
	let mut s = 0.0;
	for part in rest.split(':') {
		s = s * 60.0 + part.parse::<f64>().ok()?;
	}
	Some(days * 86_400.0 + s)
}

/// The PID listening on the port of a `ws://` URL: `POLKAMETER_NODE_PID`, else `lsof`, else `ss`.
pub async fn find_pid(ws_url: &str) -> Option<u32> {
	if let Ok(pid) = std::env::var("POLKAMETER_NODE_PID") {
		return pid.parse().ok();
	}
	let port = ws_url.rsplit_once(':')?.1.split('/').next()?;
	if let Ok(out) = Command::new("lsof")
		.args(["-t", "-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
		.output()
		.await && let Some(pid) = String::from_utf8_lossy(&out.stdout)
		.lines()
		.next()
		.and_then(|l| l.trim().parse().ok())
	{
		return Some(pid);
	}
	if let Ok(out) = Command::new("ss").args(["-Hltnp", &format!("sport = :{port}")]).output().await
	{
		let text = String::from_utf8_lossy(&out.stdout);
		if let Some(i) = text.find("pid=") {
			let digits: String = text[i + 4..].chars().take_while(char::is_ascii_digit).collect();
			if let Ok(pid) = digits.parse() {
				return Some(pid);
			}
		}
	}
	None
}

/// The process sampler.
#[derive(Debug)]
pub struct NodeSampler {
	pid: u32,
	out: JsonlWriter,
	/// The last sample: when, and the node's CPU seconds then.
	last: Option<(Millis, f64)>,
}

impl NodeSampler {
	/// Samples process `pid` into `out` (`node.jsonl`).
	pub fn new(pid: u32, out: JsonlWriter) -> Self {
		Self { pid, out, last: None }
	}

	/// Samples every 5 s until `stop`, then writes the file out.
	pub async fn run(mut self, stop: CancellationToken) -> Result<(), FileError> {
		let mut every = tokio::time::interval(EVERY);
		loop {
			tokio::select! {
				() = stop.cancelled() => break,
				_ = every.tick() => self.sample().await?,
			}
		}
		self.out.finish()
	}

	async fn sample(&mut self) -> Result<(), FileError> {
		// No answer: the process is gone, and the node-down rule reports it.
		let Ok(out) = Command::new("ps")
			.args(["-o", "time=,rss=", "-p", &self.pid.to_string()])
			.output()
			.await
		else {
			return Ok(());
		};
		let text = String::from_utf8_lossy(&out.stdout);
		let mut parts = text.split_whitespace();
		let (Some(time), Some(rss)) = (parts.next(), parts.next()) else { return Ok(()) };
		let (Some(cpu_s), Ok(rss_kib)) = (cpu_seconds(time), rss.parse::<f64>()) else {
			return Ok(());
		};
		let t = now_ms();
		let cpu_pct = self
			.last
			.map(|(at, was)| 100.0 * (cpu_s - was) / (t.saturating_sub(at).max(1) as f64 / 1000.0));
		self.last = Some((t, cpu_s));
		self.out.write(&NodeSample {
			t,
			cpu_pct: cpu_pct.map(|p| p.round() as i64),
			rss_mi_b: (rss_kib / 1024.0).round() as u64,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ps_times_parse() {
		assert_eq!(cpu_seconds("1:02.50"), Some(62.5));
		assert_eq!(cpu_seconds("01:02:03"), Some(3723.0));
		assert_eq!(cpu_seconds("2-01:00:00"), Some(2.0 * 86_400.0 + 3600.0));
		assert_eq!(cpu_seconds("x"), None);
	}
}
