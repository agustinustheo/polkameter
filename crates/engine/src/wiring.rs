//! Starts the monitors next to the load and connects them: run events to the scraper, tool
//! errors to the load loop. They stop in stages, as the run needs their files: the process
//! sampler before the step records are written, the chain recorders after the loss check, the
//! rest at the end. The load crate knows nothing of the monitors.

use std::future::Future;

use anyhow::Context as _;
use polkameter_chain::Client;
use polkameter_files::{Problems, RunDir};
use polkameter_load::runner::RunEvent;
use polkameter_monitors::{
	MonitorError, Scraper, ScraperHandle, Target, chain_series, process, relay,
};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{plan::Plan, plugins::Registry};

type Task = JoinHandle<Result<(), MonitorError>>;

/// The targets of the plan's monitors, read from the topology this host registered under the plan's
/// alias. `None` when the plan has no monitors.
pub fn monitor_targets(plan: &Plan, registry: &Registry) -> anyhow::Result<Option<Vec<Target>>> {
	let Some(monitors) = &plan.monitors else { return Ok(None) };
	let topology = registry.topologies.get(&monitors.topology).with_context(|| {
		format!("topology alias {} is not installed on this host", monitors.topology)
	})?;
	Ok(Some(polkameter_monitors::load_targets(topology, monitors.para_id)?))
}

/// Where the monitors read from.
pub struct Chains<'a> {
	/// WebSocket URL of the node the load is submitted to: its process is sampled.
	pub node_url: &'a str,
	/// The relay's WebSocket URL, for the relay recorder.
	pub relay_url: Option<&'a str>,
}

/// The running monitors.
pub struct Monitors {
	/// The scraper, for the loss check's pool read.
	pub scraper: ScraperHandle,
	/// What the monitors could not record.
	pub problems: Problems,
	/// Cancelled when a monitor fails with an error of our own: the load loop stops at once.
	pub tool_failed: CancellationToken,
	stop_sampling: CancellationToken,
	stop_chain: CancellationToken,
	stop: CancellationToken,
	sampler: Option<Task>,
	walkers: Vec<Task>,
	chain: Option<Task>,
	rest: Vec<Task>,
}

/// Runs a monitor; its own error cancels `failed` so the load loop sees it.
fn spawn<F, E>(fut: F, failed: &CancellationToken) -> Task
where
	F: Future<Output = Result<(), E>> + Send + 'static,
	E: Into<MonitorError>,
{
	let failed = failed.clone();
	tokio::spawn(async move {
		let r = fut.await.map_err(Into::into);
		if r.is_err() {
			failed.cancel();
		}
		r
	})
}

/// Step edges: scrape now, so a step window starts and ends on fresh samples.
async fn step_edges(
	mut rx: broadcast::Receiver<RunEvent>,
	scraper: ScraperHandle,
	stop: CancellationToken,
) -> Result<(), MonitorError> {
	loop {
		tokio::select! {
			() = stop.cancelled() => return Ok(()),
			e = rx.recv() => match e {
				Ok(RunEvent::StepEdge(_)) => { scraper.scrape_now().await; }
				// A missed step edge makes a step window wrong: an error of our tools.
				Err(broadcast::error::RecvError::Lagged(n)) => return Err(MonitorError::Tool(format!("run events lagged by {n}: step edges missed"))),
				Err(broadcast::error::RecvError::Closed) => return Ok(()),
			}
		}
	}
}

impl Monitors {
	/// Starts every monitor. A relay that doesn't answer or a chain without a Members pallet
	/// leaves that recorder out, as a problem (a result), not an error.
	pub async fn start(
		dir: &RunDir,
		targets: Vec<Target>,
		events: &broadcast::Sender<RunEvent>,
		chains: Chains<'_>,
	) -> Result<Self, MonitorError> {
		let problems = Problems::default();
		let [failed, stop, stop_sampling, stop_chain] =
			std::array::from_fn(|_| CancellationToken::new());

		let (scraper, handle) =
			Scraper::new(targets, dir.jsonl("scrapes.jsonl")?, problems.clone());
		let rest = vec![
			spawn(scraper.run(stop.clone()), &failed),
			spawn(step_edges(events.subscribe(), handle.clone(), stop.clone()), &failed),
		];

		let sampler = match process::find_pid(chains.node_url).await {
			Some(pid) => Some(spawn(
				process::NodeSampler::new(pid, dir.jsonl("node.jsonl")?).run(stop_sampling.clone()),
				&failed,
			)),
			None => {
				eprintln!(
					"node: no PID for the node under load (set POLKAMETER_NODE_PID); no CPU or memory recorded"
				);
				None
			},
		};

		// The chain recorders share one writer of chain.jsonl; it ends when both are gone.
		let (series, ops) = chain_series::channel();
		let chain = Some(spawn(chain_series::run(dir.jsonl("chain.jsonl")?, ops), &failed));
		let mut walkers = Vec::new();
		if let Some(relay_url) = chains.relay_url {
			match Client::connect(relay_url).await {
				Ok(client) => walkers.push(spawn(
					relay::walk(client, series.clone(), stop_chain.clone(), problems.clone()),
					&failed,
				)),
				Err(e) => problems
					.record(format!("relay recorder: cannot connect to {} ({e})", relay_url)),
			}
		}
		Ok(Self {
			scraper: handle,
			problems,
			tool_failed: failed,
			stop_sampling,
			stop_chain,
			stop,
			sampler,
			walkers,
			chain,
			rest,
		})
	}

	/// Stops the process sampler, so `node.jsonl` is complete for the step records.
	pub async fn stop_sampling(&mut self) -> Result<(), MonitorError> {
		halt(&self.stop_sampling, self.sampler.take()).await
	}

	/// Stops the chain recorders; each walks the last finalized block it saw first.
	pub async fn stop_chain(&mut self) -> Result<(), MonitorError> {
		halt(&self.stop_chain, self.walkers.drain(..).chain(self.chain.take())).await
	}

	/// Stops everything that still runs (the scraper takes one last sample) and returns the
	/// first error of our own.
	pub async fn stop(mut self) -> Result<(), MonitorError> {
		self.stop_sampling.cancel();
		self.stop_chain.cancel();
		self.stop.cancel();
		let tasks = self
			.sampler
			.take()
			.into_iter()
			.chain(self.walkers.drain(..))
			.chain(self.chain.take())
			.chain(self.rest.drain(..));
		let mut first = Ok(());
		for t in tasks {
			first = first.and(join(t).await);
		}
		first
	}
}

/// Cancels a monitor's token, then waits for its tasks; the first error is returned.
async fn halt(
	token: &CancellationToken,
	tasks: impl IntoIterator<Item = Task>,
) -> Result<(), MonitorError> {
	token.cancel();
	for t in tasks {
		join(t).await?;
	}
	Ok(())
}

async fn join(t: Task) -> Result<(), MonitorError> {
	t.await
		.unwrap_or_else(|e| Err(MonitorError::Tool(format!("monitor task ended abnormally: {e}"))))
}

impl Drop for Monitors {
	fn drop(&mut self) {
		self.stop_sampling.cancel();
		self.stop_chain.cancel();
		self.stop.cancel();
	}
}
