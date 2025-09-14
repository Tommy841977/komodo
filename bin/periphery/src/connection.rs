use std::{sync::OnceLock, time::Duration};

use axum::{
  extract::{WebSocketUpgrade, ws::Message},
  http::StatusCode,
  response::Response,
};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use periphery_client::message::{
  MessageState, from_transport_bytes, to_transport_bytes,
};
use resolver_api::Resolve;
use response::JsonBytes;
use serror::{AddStatusCode, serialize_error_bytes};
use tokio::sync::{Mutex, mpsc::Sender};
use transport::{BufferedReceiver, buffered_channel};

use crate::api::PeripheryRequest;

pub async fn inbound_connection(
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  // Limits to only one active websocket connection.
  let mut response_receiver = response_receiver()
    .try_lock()
    .status_code(StatusCode::FORBIDDEN)?;

  Ok(ws.on_upgrade(|socket| async move {
    // TODO: Handle authentication exchange.

    let (mut ws_write, mut ws_read) = socket.split();

    let ws_read = async {
      // Listen for incoming requests from Core
      loop {
        match ws_read.next().await {
          // Incoming core msg
          Some(Ok(Message::Binary(msg))) => handle_msg(msg),
          // Disconnection cases.
          Some(Ok(Message::Close(frame))) => {
            warn!("Core connection closed with frame: {frame:?}");
            break;
          }
          None => break,
          Some(Err(e)) => {
            error!("Failed to read Core websocket message | {e:?}");
            break;
          }
          // Can ignore the rest
          _ => {
            continue;
          }
        };
      }
    };

    let ws_write = async {
      // Write the messages from Periphery back to Core.
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
            // TODO: stop read loop
            break;
          }
        }
      }
    };

    // ENSURE: Graceful disconnects
    tokio::select! {
      _ = ws_read => {},
      _ = ws_write => {}
    };
  }))
}

// Creates an execution thread to process the request.
fn handle_msg(msg: Bytes) {
  tokio::spawn(async move {
    let (id, request) =
      match from_transport_bytes::<PeripheryRequest>(&msg) {
        Ok((id, _, Some(request))) => (id, request),
        Ok(_) => {
          // No data, ignore
          return;
        }
        Err(e) => {
          // TODO: handle:
          return;
        }
      };

    let resolve_response = async {
      let (state, data) =
        match request.resolve(&crate::api::Args).await {
          Ok(JsonBytes::Ok(res)) => (MessageState::Successful, res),
          Ok(JsonBytes::Err(e)) => (
            MessageState::Failed,
            serialize_error_bytes(
              &anyhow::Error::new(e)
                .context("Failed to serialize response body"),
            ),
          ),
          Err(e) => {
            (MessageState::Failed, serialize_error_bytes(&e.error))
          }
        };
      if let Err(e) = response_sender()
        .send(to_transport_bytes(id, state, &data))
        .await
      {
        error!("Failed to send response over channel | {e:?}");
      }
    };

    let ping_in_progress = async {
      loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Err(e) = response_sender()
          .send(to_transport_bytes(id, MessageState::InProgress, &[]))
          .await
        {
          error!("Failed to ping in progress over channel | {e:?}");
        }
      }
    };

    tokio::select! {
      _ = resolve_response => {},
      _ = ping_in_progress => {},
    }
  });
}

static RESPONSE_SENDER: OnceLock<Sender<Vec<u8>>> = OnceLock::new();
fn response_sender() -> &'static Sender<Vec<u8>> {
  RESPONSE_SENDER
    .get()
    .expect("response_sender accessed before initialized")
}

static RESPONSE_RECEIVER: OnceLock<Mutex<BufferedReceiver>> =
  OnceLock::new();
fn response_receiver() -> &'static Mutex<BufferedReceiver> {
  RESPONSE_RECEIVER
    .get()
    .expect("response_receiver accessed before initialized")
}

const RESPONSE_BUFFER_MAX_LEN: usize = 1_024;

/// Must call in startup sequence
pub fn init_response_channel() {
  let (sender, receiver) = buffered_channel(RESPONSE_BUFFER_MAX_LEN);
  RESPONSE_SENDER
    .set(sender)
    .expect("response_sender initialized more than once");
  RESPONSE_RECEIVER
    .set(Mutex::new(receiver))
    .expect("response_receiver initialized more than once");
}
