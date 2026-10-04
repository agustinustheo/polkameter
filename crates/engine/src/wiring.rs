//! Starts the monitors next to the load and connects them: run events to the scraper, tool
//! errors to the load loop. They stop in stages, as the run needs their files: the process
//! sampler before the step records are written, the chain recorders after the loss check (the
//! Recycler may still be working through the run's loads), the rest at the end. The load crate
//! knows nothing of the monitors.

use std::future::Future;
use std::time::Duration;

use polkameter_chain::Client;
use polkameter_files::{Problems, RunDir};
use polkameter_load::runner::RunEvent;
use polkameter_monitors::recycler::RecyclerRecorder;
use polkameter_monitors::relay::RelayRecorder;
use polkameter_monitors::{
	MonitorError, Scraper, ScraperHandle, Target, chain_series, process, walker,
};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

type Task = JoinHandle<Result<(), MonitorError>>;

/// Where the monitors read from.
pub struct Chains<'a> {
	/// People.
	pub people: &'a Client,
	/// People's WebSocket URL: the node whose process is sampled.
	pub people_url: &'a str,
	/// The relay's WebSocket URL, for the relay recorder.
	pub relay_url: Option<&'a str>,
	/// Keys the recycler recorder follows to a built root: a Members collection and its keys.
	pub vouchers: Option<([u8; 32], Vec<[u8; 32]>)>,
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
	/// True while the Recycler backlog is back to where the run found it.
	drained: Option<watch::Receiver<bool>>,
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
				Ok(RunEvent::Phase(_)) => {}
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
		let (failed, stop, stop_sampling, stop_chain) = (
			CancellationToken::new(),
			CancellationToken::new(),
			CancellationToken::new(),
			CancellationToken::new(),
		);

		let (scraper, handle) =
			Scraper::new(targets, dir.jsonl("scrapes.jsonl")?, problems.clone());
		let rest = vec![
			spawn(scraper.run(stop.clone()), &failed),
			spawn(step_edges(events.subscribe(), handle.clone(), stop.clone()), &failed),
		];

		let sampler = match process::find_pid(chains.people_url).await {
			Some(pid) => Some(spawn(
				process::NodeSampler::new(pid, dir.jsonl("node.jsonl")?).run(stop_sampling.clone()),
				&failed,
			)),
			None => {
				eprintln!("node: no People node PID (set PEOPLE_PID); no CPU or memory recorded");
				None
			},
		};

		// The chain recorders share one writer of chain.jsonl; it ends when both are gone.
		let (series, ops) = chain_series::channel();
		let chain = Some(spawn(chain_series::run(dir.jsonl("chain.jsonl")?, ops), &failed));
		let mut walkers = Vec::new();
		if let Some(relay_url) = chains.relay_url {
			match Client::connect(relay_url).await {
				Ok(relay) => walkers.push(spawn(
					walker::walk(
						relay.clone(),
						RelayRecorder::new(relay, series.clone()),
						stop_chain.clone(),
						problems.clone(),
					),
					&failed,
				)),
				Err(e) => problems
					.record(format!("relay recorder: cannot connect to {} ({e})", relay_url)),
			}
		}
		let mut drained = None;
		if chains.relay_url.is_some() {
			match RecyclerRecorder::new(chains.people.clone(), series, chains.vouchers).await {
				Ok(Some((recorder, rx))) => {
					drained = Some(rx);
					walkers.push(spawn(
						walker::walk(
							chains.people.clone(),
							recorder,
							stop_chain.clone(),
							problems.clone(),
						),
						&failed,
					));
				},
				Ok(None) => {},
				Err(e) => problems.record(format!("recycler recorder: cannot start ({e})")),
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
			drained,
		})
	}

	/// Stops the process sampler, so `node.jsonl` is complete for the step records.
	pub async fn stop_sampling(&mut self) -> Result<(), MonitorError> {
		self.stop_sampling.cancel();
		join(self.sampler.take()).await
	}

	/// Lets the Recycler get back to where it started (up to `wait`), then stops the chain
	/// recorders; each walks the last finalized block it saw first.
	pub async fn stop_chain(&mut self, wait: Duration) -> Result<(), MonitorError> {
		if let Some(mut rx) = self.drained.take() {
			let _ = tokio::time::timeout(wait, rx.wait_for(|d| *d)).await;
		}
		self.stop_chain.cancel();
		for t in self.walkers.drain(..) {
			join(Some(t)).await?;
		}
		join(self.chain.take()).await
	}

	/// Stops everything that still runs (the scraper takes one last sample) and returns the
	/// first error of our own.
	pub async fn stop(mut self) -> Result<(), MonitorError> {
		self.stop_sampling.cancel();
		self.stop_chain.cancel();
		self.stop.cancel();
		let mut first = Ok(());
		let tasks = self
			.sampler
			.take()
			.into_iter()
			.chain(self.walkers.drain(..))
			.chain(self.chain.take())
			.chain(self.rest.drain(..));
		for t in tasks {
			if let Err(e) = join(Some(t)).await
				&& first.is_ok()
			{
				first = Err(e);
			}
		}
		first
	}
}

async fn join(t: Option<Task>) -> Result<(), MonitorError> {
	match t {
		Some(t) => t.await.expect("a monitor task does not panic"),
		None => Ok(()),
	}
}

impl Drop for Monitors {
	fn drop(&mut self) {
		self.stop_sampling.cancel();
		self.stop_chain.cancel();
		self.stop.cancel();
	}
}
