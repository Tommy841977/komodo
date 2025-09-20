use std::time::Duration;

use anyhow::Context;
use axum::http::HeaderValue;
use bytes::Bytes;
use transport::{
  auth2::{ConnectionIdentifiers, handle_client_side_login},
  fix_ws_address,
  websocket::{
    WebsocketMessage, WebsocketReceiver, WebsocketSender,
    tungstenite::TungsteniteWebsocket,
  },
};

use crate::connection::{handle_incoming_bytes, ws_receiver};

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
  let url = format!(
    "{core_host}/ws/periphery?server={}",
    urlencoding::encode(connect_as)
  );
  let parsed_url =
    ::url::Url::parse(&url).context("Failed to parse ws url")?;
  let host: Vec<u8> = parsed_url
    .host()
    .context("url has no host")?
    .to_string()
    .into();
  let query =
    parsed_url.query().context("url has no query")?.as_bytes();

  info!("Initiating outbound connection to {url}");

  loop {
    let (mut socket, accept) = match connect_websocket(&url).await {
      Ok(res) => res,
      Err(e) => {
        warn!("{e:#}");
        tokio::time::sleep(Duration::from_secs(5)).await;
        continue;
      }
    };

    info!("Connected to core connection websocket");

    let id = ConnectionIdentifiers {
      host: &host,
      accept: accept.as_bytes(),
      query,
    };

    // TODO: source the pk
    if let Err(e) =
      handle_client_side_login(&mut socket, id, b"RANDOM_PRIVATE_KEY")
        .await
    {
      warn!("Failed to login | {e:#}");
      tokio::time::sleep(Duration::from_secs(5)).await;
      continue;
    };

    info!("Logged in to core connection websocket");

    let (mut ws_write, mut ws_read) = socket.split();

    let forward_writes = async {
      loop {
        let msg = match write_receiver.recv().await {
          // Sender Dropped (shouldn't happen, it is static).
          None => break,
          // This has to copy the bytes to follow ownership rules.
          Some(msg) => Bytes::copy_from_slice(msg),
        };
        match ws_write.send(msg).await {
          // Clears the stored message from receiver buffer.
          // TODO: Move after response ack.
          Ok(_) => write_receiver.clear_buffer(),
          Err(e) => {
            warn!("Failed to send response | {e:?}");
            let _ = ws_write.close(None).await;
            break;
          }
        }
      }
    };

    let handle_reads = async {
      loop {
        match ws_read.recv().await {
          Ok(WebsocketMessage::Binary(bytes)) => {
            handle_incoming_bytes(bytes).await
          }
          Ok(WebsocketMessage::Close(frame)) => {
            warn!("Connection closed with frame: {frame:?}");
            break;
          }
          Ok(WebsocketMessage::Closed) => {
            warn!("Connection already closed");
            break;
          }
          Err(e) => {
            error!("Failed to read websocket message | {e:?}");
            break;
          }
        };
      }
    };

    tokio::select! {
      _ = forward_writes => {},
      _ = handle_reads => {},
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
    .context("sec-websocket-accept")?;
  Ok((TungsteniteWebsocket(ws), accept))
}
