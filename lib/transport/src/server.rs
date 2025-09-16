use axum::{
  extract::{WebSocketUpgrade, ws::Message},
  http::StatusCode,
  response::Response,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serror::AddStatusCode;
use tokio::sync::Mutex;
use tracing::{error, warn};

use crate::channel::BufferedReceiver;

pub fn inbound_connection(
  ws: WebSocketUpgrade,
  handle_read: impl Fn(Bytes) + Send + Sync + 'static,
  write_receiver: &'static Mutex<BufferedReceiver<Bytes>>,
) -> serror::Result<Response> {
  // Limits to only one active websocket connection.
  let mut response_receiver = write_receiver
    .try_lock()
    .status_code(StatusCode::FORBIDDEN)?;

  Ok(ws.on_upgrade(|socket| async move {
    // TODO: Handle authentication exchange.

    let (mut ws_write, mut ws_read) = socket.split();

    // Handle incoming messages
    let ws_read = async {
      loop {
        match ws_read.next().await {
          // Incoming core msg
          Some(Ok(Message::Binary(msg))) => handle_read(msg),
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

    // Handle outgoing messages
    let ws_write = async {
      loop {
        let msg = match response_receiver.recv().await {
          // Sender Dropped (shouldn't happen, it is static).
          None => break,
          // This has to copy the bytes to follow ownership rules.
          Some(msg) => Message::Binary(Bytes::copy_from_slice(msg)),
        };
        match ws_write.send(msg).await {
          // Clears the stored message from receiver buffer.
          // TODO: Move after response ack from Core.
          Ok(_) => response_receiver.clear_buffer(),
          Err(e) => {
            warn!("Failed to send response | {e:?}");
            let _ = ws_write.close().await;
            break;
          }
        }
      }
    };

    tokio::select! {
      _ = ws_read => {},
      _ = ws_write => {}
    };
  }))
}
