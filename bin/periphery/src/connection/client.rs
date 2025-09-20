use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use transport::{
  auth::handle_client_side_login,
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

  info!("Initiating outbound connection to {url}");

  loop {
    let mut socket = match tokio_tungstenite::connect_async(&url)
      .await
    {
      Ok((socket, _)) => TungsteniteWebsocket(socket),
      Err(e) => {
        warn!("failed to connect to websocket | url: {url} | {e:?}");
        tokio::time::sleep(Duration::from_secs(5)).await;
        continue;
      }
    };

    info!("Connected to core connection websocket");

    if let Err(e) =
      handle_client_side_login(&mut socket, Bytes::new()).await
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
