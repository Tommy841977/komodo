use std::time::Duration;

use anyhow::Context;
use axum::http::HeaderValue;
use transport::{
  auth::{ClientLoginFlow, ConnectionIdentifiers},
  fix_ws_address,
  websocket::tungstenite::TungsteniteWebsocket,
};
use url::Url;

use crate::connection::ws_receiver;

pub async fn handler(
  core_host: &str,
  connect_as: &str,
) -> anyhow::Result<()> {
  if let Err(e) =
    rustls::crypto::aws_lc_rs::default_provider().install_default()
  {
    error!("Failed to install default crypto provider | {e:?}");
    std::process::exit(1);
  };

  let mut write_receiver = ws_receiver()
    .try_lock()
    .context("Websocket handler called more than once.")?;

  let core_host = fix_ws_address(core_host);
  let core_endpoint = format!(
    "{core_host}/ws/periphery?server={}",
    urlencoding::encode(connect_as)
  );
  let url =
    Url::parse(&core_endpoint).context("Failed to parse ws url")?;
  let host: Vec<u8> =
    url.host().context("url has no host")?.to_string().into();
  let query = url.query().context("url has no query")?.as_bytes();

  info!("Initiating outbound connection to {url}");

  let mut already_logged_connection_error = false;
  let mut already_logged_login_error = false;

  loop {
    let (socket, accept) =
      match connect_websocket(&core_endpoint).await {
        Ok(res) => res,
        Err(e) => {
          if !already_logged_connection_error {
            warn!("{e:#}");
            already_logged_connection_error = true;
            // If error transitions from login to connection,
            // set to false to see login error after reconnect.
            already_logged_login_error = false;
          }
          tokio::time::sleep(Duration::from_secs(5)).await;
          continue;
        }
      };

    already_logged_connection_error = false;

    if !already_logged_login_error {
      info!("Connected to core connection websocket");
    }

    let connection_identifiers = ConnectionIdentifiers {
      host: &host,
      accept: accept.as_bytes(),
      query,
    };

    let handler = super::WebsocketHandler {
      socket,
      connection_identifiers,
      write_receiver: &mut write_receiver,
      on_login_success: || already_logged_login_error = false,
    };

    if let Err(e) = handler.handle::<ClientLoginFlow>().await {
      if !already_logged_login_error {
        warn!("Failed to login | {e:#}");
        already_logged_login_error = true;
      }
      tokio::time::sleep(Duration::from_secs(5)).await;
      continue;
    };
  }
}

async fn connect_websocket(
  url: &str,
) -> anyhow::Result<(TungsteniteWebsocket, HeaderValue)> {
  let (ws, mut response) = tokio_tungstenite::connect_async(url)
    .await
    .with_context(|| format!("Failed to connect to {url}"))?;
  let accept = response
    .headers_mut()
    .remove("sec-websocket-accept")
    .context("sec-websocket-accept")
    .context("Headers do not contain Sec-Websocket-Accept")?;
  Ok((TungsteniteWebsocket(ws), accept))
}
