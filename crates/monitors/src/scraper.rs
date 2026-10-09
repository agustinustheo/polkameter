//! Scrapes `/metrics` of every node every few seconds and whenever asked (step edges, the loss
//! check). Only the families in the registry are kept, with their `# HELP` and `# TYPE` lines.
//!
//! Each scrape of each node is one [`ScrapeRecord`] in `scrapes.jsonl`; `t` is when that node's
//! answer arrived. A node that doesn't answer is a record with `error`, never a stop.

use std::{collections::HashMap, time::Duration};

use polkameter_files::{
	FileError, JsonlWriter, ScrapeRecord, now_ms, parse_sample_line,
	registry::{self, NODE_METRICS},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use polkameter_files::Problems;

use crate::{Job, Target};

const EVERY: Duration = Duration::from_secs(5);

/// One good scrape of one node: series (name plus labels, as served) to value.
#[derive(Debug, Clone, Default)]
pub struct Sample {
	/// When it arrived.
	pub t: u64,
	/// Series to value.
	pub metrics: HashMap<String, f64>,
}

impl Sample {
	/// Sum of every series of `name`, whatever its labels; `None` when it is missing.
	pub fn sum(&self, name: &str) -> Option<f64> {
		let mut values = self
			.metrics
			.iter()
			.filter(|(k, _)| *k == name || k.starts_with(&format!("{name}{{")))
			.map(|(_, v)| *v)
			.peekable();
		values.peek()?;
		Some(values.sum())
	}
}

/// Keeps the lines of the families we read; returns the kept text and its samples.
pub fn filter_exposition(text: &str) -> (String, HashMap<String, f64>) {
	let mut kept = Vec::new();
	let mut metrics = HashMap::new();
	for line in text.lines() {
		if let Some(rest) = line.strip_prefix("# ") {
			let mut parts = rest.split(' ');
			let (kind, name) = (parts.next(), parts.next());
			if matches!(kind, Some("TYPE" | "HELP"))
				&& name.is_some_and(|n| NODE_METRICS.iter().any(|d| d.name == n))
			{
				kept.push(line);
			}
			continue;
		}
		let Some(p) = parse_sample_line(line) else { continue };
		if registry::node_family_of(&p.name).is_none() {
			continue;
		}
		kept.push(line);
		if let Some((series, _)) = line.rsplit_once(' ') {
			metrics.insert(series.to_owned(), p.value);
		}
	}
	(kept.join("\n"), metrics)
}

type Scrapes = HashMap<String, Option<Sample>>;

/// Talks to a running scraper.
#[derive(Debug, Clone)]
pub struct ScraperHandle {
	now: mpsc::Sender<oneshot::Sender<Scrapes>>,
	/// The first collator's last good sample.
	pub collator: watch::Receiver<Option<Sample>>,
}

impl ScraperHandle {
	/// Scrapes every node now; `None` for a node whose scrape failed (an old sample would give
	/// wrong numbers at a step edge).
	pub async fn scrape_now(&self) -> Scrapes {
		let (tx, rx) = oneshot::channel();
		if self.now.send(tx).await.is_err() {
			return Scrapes::new();
		}
		rx.await.unwrap_or_default()
	}
}

/// The scraper task.
#[derive(Debug)]
pub struct Scraper {
	targets: Vec<Target>,
	out: JsonlWriter,
	http: reqwest::Client,
	problems: Problems,
	failures: HashMap<String, u32>,
	/// The instance name of the first collator: its sample goes to `collator`.
	collator_instance: Option<String>,
	collator: watch::Sender<Option<Sample>>,
	now: mpsc::Receiver<oneshot::Sender<Scrapes>>,
}

impl Scraper {
	/// A scraper of `targets` writing to `out`, and its handle.
	pub fn new(
		targets: Vec<Target>,
		out: JsonlWriter,
		problems: Problems,
	) -> (Self, ScraperHandle) {
		let (collator, collator_rx) = watch::channel(None);
		let (now_tx, now) = mpsc::channel(16);
		let collator_instance =
			targets.iter().find(|t| t.job == Job::Collator).map(|t| t.instance.clone());
		let s = Self {
			targets,
			out,
			http: http_client(),
			problems,
			failures: HashMap::new(),
			collator_instance,
			collator,
			now,
		};
		(s, ScraperHandle { now: now_tx, collator: collator_rx })
	}

	/// Scrapes every 5 s and on request until `stop`, then once more.
	pub async fn run(mut self, stop: CancellationToken) -> Result<(), FileError> {
		let mut every = tokio::time::interval(EVERY);
		loop {
			tokio::select! {
				() = stop.cancelled() => break,
				_ = every.tick() => { self.scrape().await?; }
				Some(reply) = self.now.recv() => { let _ = reply.send(self.scrape().await?); }
			}
		}
		self.scrape().await?;
		self.out.flush()
	}

	async fn scrape(&mut self) -> Result<Scrapes, FileError> {
		let answers =
			futures_util::future::join_all(self.targets.iter().map(|t| fetch(&self.http, t))).await;
		let mut out = Scrapes::new();
		for (target, answer) in self.targets.iter().zip(answers) {
			let t = now_ms();
			let (job, instance) = (target.job.label().to_owned(), target.instance.clone());
			match answer {
				Ok(body) => {
					let (text, metrics) = filter_exposition(&body);
					self.out.write(&ScrapeRecord {
						t,
						job,
						instance: instance.clone(),
						text: Some(text),
						error: None,
					})?;
					let sample = Sample { t, metrics };
					if self.collator_instance.as_deref() == Some(instance.as_str()) {
						let _ = self.collator.send(Some(sample.clone()));
					}
					out.insert(instance, Some(sample));
				},
				Err(error) => {
					self.out.write(&ScrapeRecord {
						t,
						job,
						instance: instance.clone(),
						text: None,
						error: Some(error.clone()),
					})?;
					let n = self.failures.entry(instance.clone()).or_default();
					*n += 1;
					if *n == 1 {
						self.problems.record(format!(
							"scraper: cannot read {instance} at {} ({error})",
							target.url
						));
					}
					out.insert(instance, None);
				},
			}
		}
		Ok(out)
	}
}

/// The HTTP client of every read of a node: a 5 s timeout.
pub(crate) fn http_client() -> reqwest::Client {
	reqwest::Client::builder()
		.timeout(Duration::from_secs(5))
		.build()
		.expect("http client")
}

/// GET `url`; a transport error or an error status (4xx, 5xx) is the error.
pub(crate) async fn get(
	http: &reqwest::Client,
	url: &str,
) -> Result<reqwest::Response, reqwest::Error> {
	http.get(url).send().await?.error_for_status()
}

/// A node's `/metrics` body, or why it could not be read.
async fn fetch(http: &reqwest::Client, target: &Target) -> Result<String, String> {
	let res = http.get(&target.url).send().await.map_err(|e| e.to_string())?;
	if !res.status().is_success() {
		return Err(format!("HTTP {}", res.status().as_u16()));
	}
	res.text().await.map_err(|e| e.to_string())
}
