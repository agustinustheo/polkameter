//! `polkameter-substrate-plugin`: the substrate plugin's executable. Install it with
//! `polkameter plugin install`.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	polkameter_plugin_sdk::serve(polkameter_substrate_plugin::Substrate::default()).await
}
