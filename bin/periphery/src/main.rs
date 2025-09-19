#[macro_use]
extern crate tracing;

mod api;
mod config;
mod connection;
mod docker;
mod helpers;
mod server;
mod stats;
mod terminal;

async fn app() -> anyhow::Result<()> {
  dotenvy::dotenv().ok();
  let config = config::periphery_config();
  logger::init(&config.logging)?;

  info!("Komodo Periphery version: v{}", env!("CARGO_PKG_VERSION"));

  if config.pretty_startup_config {
    info!("{:#?}", config.sanitized());
  } else {
    info!("{:?}", config.sanitized());
  }

  stats::spawn_polling_thread();
  docker::stats::spawn_polling_thread();
  connection::init_response_channel();

  server::run_connection_server().await
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
