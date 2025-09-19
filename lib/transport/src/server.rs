use anyhow::{Context, anyhow};
use axum::{
  extract::{
    WebSocketUpgrade,
    ws::{Message, WebSocket},
  },
  http::StatusCode,
  response::Response,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use serror::AddStatusCode;
use tokio::sync::Mutex;
use tracing::{error, warn};

use crate::{
  MessageState, TransportHandler, channel::BufferedReceiver,
};

/// Handles server side / inbound connection
pub fn handle_server_connection<
  T: TransportHandler + Send + Sync + 'static,
>(
  ws: WebSocketUpgrade,
  transport: T,
  write_receiver: &'static Mutex<BufferedReceiver<Bytes>>,
) -> serror::Result<Response> {
  // Limits to only one active websocket connection.
  let mut write_receiver = write_receiver
    .try_lock()
    .status_code(StatusCode::FORBIDDEN)?;

  Ok(ws.on_upgrade(|mut socket| async move {
    if let Err(e) = handle_login(&mut socket, |b| true).await {
      warn!("Client failed to login | {e:#}");
      return;
    };

    let (mut ws_write, mut ws_read) = socket.split();

    let forward_writes = async {
      loop {
        let msg = match write_receiver.recv().await {
          // Sender Dropped (shouldn't happen, it is static).
          None => break,
          // This has to copy the bytes to follow ownership rules.
          Some(msg) => Message::Binary(Bytes::copy_from_slice(msg)),
        };
        match ws_write.send(msg).await {
          // Clears the stored message from receiver buffer.
          // TODO: Move after response ack.
          Ok(_) => write_receiver.clear_buffer(),
          Err(e) => {
            warn!("Failed to send response | {e:?}");
            let _ = ws_write.close().await;
            break;
          }
        }
      }
    };

    let handle_reads = async {
      loop {
        match ws_read.next().await {
          // Incoming core msg
          Some(Ok(Message::Binary(bytes))) => {
            transport.handle_incoming_bytes(bytes).await
          }
          // Disconnection cases.
          Some(Ok(Message::Close(frame))) => {
            warn!("Connection closed with frame: {frame:?}");
            break;
          }
          None => break,
          Some(Err(e)) => {
            error!("Failed to read websocket message | {e:?}");
            break;
          }
          // Can ignore the rest
          _ => {
            continue;
          }
        };
      }
    };

    tokio::select! {
      _ = forward_writes => {},
      _ = handle_reads => {},
    };
  }))
}

async fn handle_login(
  socket: &mut WebSocket,
  validate: impl Fn(&[u8]) -> bool,
) -> anyhow::Result<()> {
  loop {
    // Poll for next message
    let msg = socket
      .try_next()
      .await
      .context("Failed to receive login credentials")?
      .context("Stream broken before login credentials received")?;
    // Treat first message as credentials
    let credentials = match &msg {
      Message::Text(text) => text.as_bytes(),
      Message::Binary(bytes) => &bytes,
      Message::Close(frame) => {
        return Err(anyhow!(
          "Websocket close frame received during login | frame: {frame:?}"
        ));
      }
      // Ignore others
      _ => continue,
    };
    // Validate
    if validate(credentials) {
      // Send login confirmation
      socket
        .send(Message::Binary(MessageState::Successful.into()))
        .await?;
      return Ok(());
    } else {
      // Send login failure
      socket
        .send(Message::Binary(MessageState::Failed.into()))
        .await?;
      let _ = socket.close().await;
      return Err(anyhow!("Received invalid credentials"));
    }
  }
}
