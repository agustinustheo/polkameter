//! The machine the load tool runs on, for the summary: CPUs and CPU model.

use std::process::Command;

use polkameter_files::summary::Runner;

fn sysctl(key: &str) -> Option<String> {
	let out = Command::new("sysctl")
		.args(["-n", key])
		.output()
		.ok()
		.filter(|o| o.status.success())?;
	Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn proc_field(path: &str, key: &str) -> Option<String> {
	let text = std::fs::read_to_string(path).ok()?;
	let line = text.lines().find(|l| l.starts_with(key))?;
	Some(line.split_once(':')?.1.trim().to_owned())
}

/// This machine.
pub fn runner() -> Runner {
	let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
	let cpu_model = if cfg!(target_os = "macos") {
		sysctl("machdep.cpu.brand_string")
	} else {
		proc_field("/proc/cpuinfo", "model name")
	};
	Runner { cpus, cpu_model }
}
