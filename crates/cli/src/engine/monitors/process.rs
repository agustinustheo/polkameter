//! Samples the CPU and memory of the node under load with `ps` every few seconds, since the node
//! exports no process metrics on macOS. CPU is the change in the node's CPU time over wall
//! time, so 250% means 2.5 cores busy.
//!
//! Only a node on this machine can be sampled. The process is `POLKAMETER_NODE_PID`, or the one
//! listening on the RPC port of a loopback URL (`localhost`, `127.0.0.1`, `::1`) that also
//! listens on one of the node's own p2p ports (`system_localListenAddresses`): a proxy on the
//! RPC port (Node.js, Python and Go proxies have no name to tell them by) does not, so it is not
//! taken for the node. For a remote URL nothing is sampled. In both cases the run goes on without
//! CPU or memory, with a warning.
//!
//! When the sampled process ends, the sampler looks for the node process now on the same port,
//! records the change as a problem and samples the new one; the CPU of its first sample is not
//! known.
//!
//! The sampler also tells the load what the node's process shows ([`NodeProcess`]): that it
//! ended, that it is stopped, or that it serves the RPC port itself. With one of those, a node
//! that can't be reached is node down; without, it is the node or the network between.

use std::time::Duration;

use polkameter_plugin_sdk::observers::{
	FileError, JsonlWriter, Millis, NodeSample, Problems, now_ms,
};

use crate::load::runner::NodeProcess;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const EVERY: Duration = Duration::from_secs(5);

/// Processes that listen on a port for another machine or container: never the node.
const FORWARDERS: &[&str] = &[
	"ssh",
	"sshd",
	"autossh",
	"kubectl",
	"socat",
	"nc",
	"ncat",
	"docker-proxy",
	"com.docker.backend",
	"vpnkit",
	"gvproxy",
	"limactl",
	"rootlessport",
	"slirp4netns",
	"stunnel",
	"cloudflared",
	"ngrok",
	"haproxy",
	"nginx",
	"envoy",
];

/// True for a process name that is a port forwarder, not a node.
pub fn is_forwarder(name: &str) -> bool {
	let base = name.trim().rsplit('/').next().unwrap_or("");
	FORWARDERS.contains(&base)
}

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

/// The RPC port of a node on this machine: `None` for a URL whose host is not loopback, since
/// a local process on that port would not be the node.
pub fn local_port(ws_url: &str) -> Option<u16> {
	let url = reqwest::Url::parse(ws_url).ok()?;
	let host = url.host_str()?;
	let ip = host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>();
	let loopback = host.eq_ignore_ascii_case("localhost") || ip.is_ok_and(|ip| ip.is_loopback());
	loopback.then(|| url.port_or_known_default()).flatten()
}

/// The TCP ports in the node's listen addresses (`/ip4/127.0.0.1/tcp/30333/p2p/…`).
pub fn p2p_ports(addresses: &[String]) -> Vec<u16> {
	let mut ports: Vec<u16> = addresses
		.iter()
		.filter_map(|a| a.split('/').skip_while(|p| *p != "tcp").nth(1)?.parse().ok())
		.collect();
	ports.sort_unstable();
	ports.dedup();
	ports
}

/// The process table, through `ps` and `lsof`/`ss`, for the node's RPC port (`None` for a
/// remote node) and its p2p ports.
pub struct Ps {
	port: Option<u16>,
	p2p: Vec<u16>,
}

/// What `ps` shows of a process: CPU seconds, resident KiB, and whether it is stopped.
struct Usage {
	cpu_s: f64,
	rss_kib: f64,
	stopped: bool,
}

impl Ps {
	/// For the node at `ws_url`, listening on the TCP ports `p2p` for its peers.
	pub fn new(ws_url: &str, p2p: Vec<u16>) -> Self {
		Self { port: local_port(ws_url), p2p }
	}

	/// What `ps` shows of `pid`; `None` when it is gone.
	async fn usage(&self, pid: u32) -> Option<Usage> {
		let out = Command::new("ps")
			.args(["-o", "time=,rss=,stat=", "-p", &pid.to_string()])
			.output()
			.await;
		let text = String::from_utf8_lossy(&out.ok()?.stdout).into_owned();
		let mut parts = text.split_whitespace();
		let (time, rss, stat) = (parts.next()?, parts.next()?, parts.next().unwrap_or(""));
		Some(Usage {
			cpu_s: cpu_seconds(time)?,
			rss_kib: rss.parse().ok()?,
			stopped: stat.starts_with('T'),
		})
	}

	/// Whether `pid` listens on the node's RPC port.
	async fn serves_rpc(&self, pid: u32) -> bool {
		match self.port {
			Some(port) => listening_pids(port).await.contains(&pid),
			None => false,
		}
	}

	/// The node process listening on the node's port, if there is one: a process there that also
	/// listens on one of the node's p2p ports. Any other is a proxy or a port forwarder.
	async fn listener(&self) -> Option<u32> {
		let port = self.port?;
		for pid in listening_pids(port).await {
			let name = process_name(pid).await.unwrap_or_default();
			if is_forwarder(&name) {
				crate::redact::diag!(
					"node: port {port} is served by {name} (PID {pid}), a port forwarder, not the node"
				);
				continue;
			}
			for p2p in &self.p2p {
				if listening_pids(*p2p).await.contains(&pid) {
					return Some(pid);
				}
			}
			crate::redact::diag!(
				"node: port {port} is served by {name} (PID {pid}), which listens on none of the node's p2p ports {:?}, so it is not the node (a proxy?)",
				self.p2p
			);
		}
		None
	}
}

/// The processes listening on TCP `port`: `lsof`, else `ss`.
async fn listening_pids(port: u16) -> Vec<u32> {
	if let Ok(out) = Command::new("lsof")
		.args(["-t", "-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
		.output()
		.await
	{
		let pids: Vec<u32> = String::from_utf8_lossy(&out.stdout)
			.lines()
			.filter_map(|l| l.trim().parse().ok())
			.collect();
		if !pids.is_empty() {
			return pids;
		}
	}
	let Ok(out) = Command::new("ss").args(["-Hltnp", &format!("sport = :{port}")]).output().await
	else {
		return Vec::new();
	};
	let text = String::from_utf8_lossy(&out.stdout);
	text.split("pid=")
		.skip(1)
		.filter_map(|s| s.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok())
		.collect()
}

/// The command name of `pid`.
async fn process_name(pid: u32) -> Option<String> {
	let out = Command::new("ps").args(["-o", "comm=", "-p", &pid.to_string()]).output().await;
	let name = String::from_utf8_lossy(&out.ok()?.stdout).trim().to_owned();
	(!name.is_empty()).then_some(name)
}

/// The node's PID: `POLKAMETER_NODE_PID`, or the node process listening on the port of a
/// loopback URL. `Err` says why there is none.
pub async fn find_pid(ws_url: &str, processes: &Ps) -> Result<u32, String> {
	if let Ok(pid) = std::env::var("POLKAMETER_NODE_PID") {
		return pid.parse().map_err(|_| format!("POLKAMETER_NODE_PID={pid} is not a PID"));
	}
	if processes.port.is_some() && processes.p2p.is_empty() {
		return Err("its p2p ports are unknown (system_localListenAddresses), so the process on its port can't be told from a proxy (set POLKAMETER_NODE_PID)".into());
	}
	if local_port(ws_url).is_none() {
		return Err(format!(
			"{} is not on this machine, so its process cannot be sampled (set POLKAMETER_NODE_PID for a local node behind another address)",
			crate::redact::endpoint(ws_url)
		));
	}
	processes
		.listener()
		.await
		.ok_or_else(|| "no node process listens on its port (set POLKAMETER_NODE_PID)".to_owned())
}

/// The process sampler.
pub struct NodeSampler {
	pid: u32,
	processes: Ps,
	out: JsonlWriter,
	problems: Problems,
	node: NodeProcess,
	/// The last sample: when, and the node's CPU seconds then.
	last: Option<(Millis, f64)>,
	/// The sampled process ended and no new one was found yet.
	lost: bool,
}

impl NodeSampler {
	/// Samples process `pid` into `out` (`node.jsonl`); a new process on the port goes to
	/// `problems`, and what the process shows to `node`.
	pub fn new(
		pid: u32,
		processes: Ps,
		out: JsonlWriter,
		problems: Problems,
		node: NodeProcess,
	) -> Self {
		Self { pid, processes, out, problems, node, last: None, lost: false }
	}

	/// Samples every 5 s until `stop`, then writes the file out.
	pub async fn run(mut self, stop: CancellationToken) -> Result<(), FileError> {
		if self.processes.serves_rpc(self.pid).await {
			self.node.serves_rpc(format!(
				"the node's process {} serves the RPC port itself on this machine, so nothing between could hold its answers",
				self.pid
			));
		}
		let mut every = tokio::time::interval(EVERY);
		loop {
			tokio::select! {
				() = stop.cancelled() => break,
				_ = every.tick() => self.sample().await?,
			}
		}
		self.out.finish()
	}

	/// One sample. When the process is gone, the node process now on the port is sampled
	/// instead.
	pub async fn sample(&mut self) -> Result<(), FileError> {
		let usage = match self.processes.usage(self.pid).await {
			Some(usage) => Some(usage),
			None => {
				self.node.ended(format!("the node's process {} ended", self.pid));
				self.replace().await
			},
		};
		let Some(Usage { cpu_s, rss_kib, stopped }) = usage else { return Ok(()) };
		self.node
			.stopped(stopped.then(|| format!("the node's process {} is stopped", self.pid)));
		let t = now_ms();
		let cpu_pct = self
			.last
			.map(|(at, was)| 100.0 * (cpu_s - was) / (t.saturating_sub(at).max(1) as f64 / 1000.0));
		self.last = Some((t, cpu_s));
		let rss_mi_b = (rss_kib / 1024.0).round() as u64;
		self.out.write(&NodeSample::new(
			t,
			cpu_pct.map(|p| p.round() as i64),
			rss_mi_b,
			Some(self.pid),
		))
	}

	/// After the sampled process ended: the node process now on its port, sampled from now on,
	/// and what `ps` shows of it; `None` while there is none.
	async fn replace(&mut self) -> Option<Usage> {
		match self.processes.listener().await {
			Some(pid) if pid != self.pid => {
				let what = format!(
					"node: process {} ended; sampling process {pid}, the node now on its port",
					self.pid
				);
				crate::redact::diag!("{what}");
				self.problems.record(what);
				(self.pid, self.last, self.lost) = (pid, None, false);
				self.processes.usage(pid).await
			},
			_ => {
				if !self.lost {
					self.lost = true;
					let what = format!(
						"node: process {} ended and no node process listens on its port; no CPU or memory until one does",
						self.pid
					);
					crate::redact::diag!("{what}");
					self.problems.record(what);
				}
				None
			},
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn a_remote_node_is_reported_without_its_endpoint_key() {
		if std::env::var_os("POLKAMETER_NODE_PID").is_some() {
			return;
		}
		let url = "wss://rpc.example/ws?apikey=NODE-SAMPLER-KEY";
		let why = find_pid(url, &Ps::new(url, vec![30333])).await.unwrap_err();
		assert!(!why.contains("NODE-SAMPLER-KEY"), "{why}");
		assert!(why.starts_with("wss://rpc.example/ws?apikey=[redacted] is not on this machine"));
	}

	#[test]
	fn p2p_ports_come_from_the_listen_addresses() {
		let addresses = [
			"/ip4/127.0.0.1/tcp/30333/p2p/12D3KooWNode",
			"/ip6/::1/tcp/30333/ws/p2p/12D3KooWNode",
			"/ip4/10.0.0.5/tcp/30334",
			"/ip4/10.0.0.5/udp/30333/quic-v1",
		];
		assert_eq!(p2p_ports(&addresses.map(String::from)), [30333, 30334]);
	}

	/// A process on the node's RPC port is the node only if it also listens on one of the node's
	/// p2p ports: a proxy on the RPC port doesn't, so its CPU and memory are not the node's, and
	/// it is no evidence about the node. (A Node.js proxy was sampled as the node.)
	#[tokio::test]
	async fn a_proxy_on_the_rpc_port_is_not_the_node() {
		if std::env::var_os("POLKAMETER_NODE_PID").is_some() {
			return;
		}
		// This test process plays the proxy: it listens on the RPC port, not on the p2p port.
		let rpc = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
		let url = format!("ws://127.0.0.1:{}", rpc.local_addr().unwrap().port());
		let p2p = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
		let p2p_port = p2p.local_addr().unwrap().port();
		drop(p2p);
		let why = find_pid(&url, &Ps::new(&url, vec![p2p_port])).await;
		assert!(why.is_err(), "the proxy was taken for the node: {why:?}");
		// It is the node once it listens on the p2p port too.
		let _p2p = std::net::TcpListener::bind(("127.0.0.1", p2p_port)).unwrap();
		let ps = Ps::new(&url, vec![p2p_port]);
		assert_eq!(find_pid(&url, &ps).await, Ok(std::process::id()));
		assert!(ps.serves_rpc(std::process::id()).await);
	}
}
