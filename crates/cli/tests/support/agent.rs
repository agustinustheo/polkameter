//! `polkameter agent serve` on a port the system picks, so parallel tests never race for one.
//!
//! Probing for a free port and then binding it leaves a gap in which another test, or any other
//! process, may take the port; a test could then reach that other process. Instead the agent
//! binds port 0 and the test reads the address the agent prints.
#![allow(dead_code)]

use std::{
	io::{BufRead as _, BufReader},
	process::{Child, Command, Stdio},
	sync::mpsc::{Receiver, RecvTimeoutError},
	time::{Duration, Instant},
};

const LISTENING: &str = "PolkaMeter: agent listening on ";

/// The agent's standard error after the line that named its address.
pub struct Stderr(Receiver<String>);

impl Stderr {
	/// The lines the agent wrote, until it closed standard error or `patience` passed.
	pub fn read(&self, patience: Duration) -> String {
		let deadline = Instant::now() + patience;
		let mut text = String::new();
		loop {
			match self.0.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
				Ok(line) => text.push_str(&line),
				Err(RecvTimeoutError::Disconnected) => return text,
				Err(RecvTimeoutError::Timeout) => {
					panic!("the agent kept standard error open: {text}")
				},
			}
		}
	}
}

/// Starts `command`, an `agent serve` without `--bind`, on a free loopback port. Returns the
/// agent, its address and the rest of its standard error once it listens.
pub fn start(command: &mut Command, patience: Duration) -> (Child, String, Stderr) {
	let mut child = command.args(["--bind", "127.0.0.1:0"]).stderr(Stdio::piped()).spawn().unwrap();
	let mut stderr = BufReader::new(child.stderr.take().unwrap());
	let (lines, received) = std::sync::mpsc::channel();
	// Keep reading even after the test stops listening, so the agent never writes to a closed pipe.
	std::thread::spawn(move || {
		let mut line = String::new();
		while stderr.read_line(&mut line).is_ok_and(|read| read > 0) {
			let _ = lines.send(std::mem::take(&mut line));
		}
	});
	let deadline = Instant::now() + patience;
	let mut before = String::new();
	loop {
		match received.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
			Ok(line) => match line.strip_prefix(LISTENING) {
				Some(address) => return (child, address.trim().to_owned(), Stderr(received)),
				None => before.push_str(&line),
			},
			Err(error) => {
				let _ = child.kill();
				let status = child.wait().unwrap();
				panic!("the agent did not listen ({error}, {status}): {before}");
			},
		}
	}
}
