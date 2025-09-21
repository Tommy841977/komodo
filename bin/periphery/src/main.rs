use anyhow::anyhow;

use crate::config::periphery_public_key;

#[macro_use]
extern crate tracing;

mod api;
mod config;
mod connection;
mod docker;
mod helpers;
mod stats;
mod terminal;

async fn app() -> anyhow::Result<()> {
  dotenvy::dotenv().ok();
  let config = config::periphery_config();
  logger::init(&config.logging)?;

  info!("Komodo Periphery version: v{}", env!("CARGO_PKG_VERSION"));
  // Init public key to crash on failure
  info!("Periphery Public Key: {}", periphery_public_key());

  if config.pretty_startup_config {
    info!("{:#?}", config.sanitized());
  } else {
    info!("{:?}", config.sanitized());
  }

  stats::spawn_polling_thread();
  docker::stats::spawn_polling_thread();
  connection::init_response_channel();

  match (&config.core_host, &config.connect_as) {
    (Some(core_host), Some(connect_as)) => {
      connection::client::handler(core_host, connect_as).await
    }
    (None, _) => connection::server::run().await,
    (Some(_), None) => Err(anyhow!(
      "Must provide 'connect_as' (PERIPHERY_CONNECT_AS) for outbound connection."
    )),
  }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let mut term_signal = tokio::signal::unix::signal(
    tokio::signal::unix::SignalKind::terminate(),
  )?;

  let app = tokio::spawn(app());

  tokio::select! {
    res = app => return res?,
    _ = term_signal.recv() => {
      info!("Exiting all active Terminals for shutdown");
      terminal::delete_all_terminals().await;
    },
  }

  Ok(())
}
