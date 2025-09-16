use std::{sync::OnceLock, time::Duration};

use axum::{extract::WebSocketUpgrade, response::Response};
use bytes::Bytes;
use resolver_api::Resolve;
use response::JsonBytes;
use serror::serialize_error_bytes;
use tokio::sync::{Mutex, mpsc::Sender};
use transport::{
  MessageState,
  bytes::{from_transport_bytes, to_transport_bytes},
  channel::{BufferedReceiver, buffered_channel},
};

use crate::api::PeripheryRequest;

static RESPONSE_SENDER: OnceLock<Sender<Bytes>> = OnceLock::new();
fn response_sender() -> &'static Sender<Bytes> {
  RESPONSE_SENDER
    .get()
    .expect("response_sender accessed before initialized")
}

static RESPONSE_RECEIVER: OnceLock<Mutex<BufferedReceiver<Bytes>>> =
  OnceLock::new();
fn response_receiver() -> &'static Mutex<BufferedReceiver<Bytes>> {
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

pub async fn inbound_connection(
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  transport::server::inbound_connection(
    ws,
    handle_msg,
    response_receiver(),
  )
}

// Creates an execution thread to process the request.
fn handle_msg(msg: Bytes) {
  tokio::spawn(async move {
    let (id, request) = match from_transport_bytes(&msg) {
      Ok((id, _, Some(request))) => (id, request),
      Ok(_) => {
        // No data, ignore
        return;
      }
      Err(e) => {
        // TODO: handle:
        warn!("Failed to parse transport bytes | {e:#}");
        return;
      }
    };

    let request =
      match serde_json::from_slice::<PeripheryRequest>(&request) {
        Ok(req) => req,
        Err(e) => {
          // TODO: handle:
          warn!("Failed to parse transport bytes | {e:#}");
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
