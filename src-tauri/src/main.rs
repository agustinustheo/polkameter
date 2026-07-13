#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
	if std::env::args().nth(1).as_deref() == Some("agent") {
		let runtime = tokio::runtime::Runtime::new().expect("agent runtime starts");
		if let Err(error) = runtime.block_on(polkameter_lib::serve_agent_from_env()) {
			eprintln!("Polkameter agent failed: {error}");
			std::process::exit(1);
		}
	} else {
		polkameter_lib::run()
	}
}
