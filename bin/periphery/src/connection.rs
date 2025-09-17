use std::{sync::OnceLock, time::Duration};

use axum::{extract::WebSocketUpgrade, response::Response};
use bytes::Bytes;
use cache::CloneCache;
use resolver_api::Resolve;
use response::JsonBytes;
use serror::serialize_error_bytes;
use tokio::sync::{Mutex, mpsc::Sender};
use tokio_util::sync::CancellationToken;
use transport::{
  MessageState, TransportHandler,
  bytes::{
    data_from_transport_bytes, id_state_from_transport_bytes,
    to_transport_bytes,
  },
  channel::{BufferedReceiver, buffered_channel},
};
use uuid::Uuid;

use crate::api::{Args, PeripheryRequest};

static WS_SENDER: OnceLock<Sender<Bytes>> = OnceLock::new();
pub fn ws_sender() -> &'static Sender<Bytes> {
  WS_SENDER
    .get()
    .expect("response_sender accessed before initialized")
}

static WS_RECEIVER: OnceLock<Mutex<BufferedReceiver<Bytes>>> =
  OnceLock::new();
fn ws_receiver() -> &'static Mutex<BufferedReceiver<Bytes>> {
  WS_RECEIVER
    .get()
    .expect("response_receiver accessed before initialized")
}

const RESPONSE_BUFFER_MAX_LEN: usize = 1_024;

/// Must call in startup sequence
pub fn init_response_channel() {
  let (sender, receiver) = buffered_channel(RESPONSE_BUFFER_MAX_LEN);
  WS_SENDER
    .set(sender)
    .expect("response_sender initialized more than once");
  WS_RECEIVER
    .set(Mutex::new(receiver))
    .expect("response_receiver initialized more than once");
}

pub async fn inbound_connection(
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  transport::server::handle_server_connection(
    ws,
    PeripheryTransportHandler,
    ws_receiver(),
  )
}

struct PeripheryTransportHandler;

impl TransportHandler for PeripheryTransportHandler {
  async fn handle_incoming_bytes(&self, bytes: Bytes) {
    // Maybe wrap all of this on tokio spawn
    let (id, state) = match id_state_from_transport_bytes(&bytes) {
      Ok(res) => res,
      Err(e) => {
        // TODO: handle:
        warn!("Failed to parse transport bytes | {e:#}");
        return;
      }
    };
    match state {
      MessageState::Request => handle_request(id, bytes),
      MessageState::Terminal => {
        handle_terminal_message(id, bytes).await
      }
      // Shouldn't be received by Periphery
      MessageState::InProgress => {}
      MessageState::Successful => {}
      MessageState::Failed => {}
    }
  }
}

fn handle_request(req_id: Uuid, bytes: Bytes) {
  tokio::spawn(async move {
    let request = match data_from_transport_bytes(bytes) {
      Ok(req) if !req.is_empty() => req,
      _ => {
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
        match request.resolve(&Args { req_id }).await {
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
      if let Err(e) = ws_sender()
        .send(to_transport_bytes(data, req_id, state))
        .await
      {
        error!("Failed to send response over channel | {e:?}");
      }
    };

    let ping_in_progress = async {
      loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Err(e) = ws_sender()
          .send(to_transport_bytes(
            Vec::new(),
            req_id,
            MessageState::InProgress,
          ))
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

pub type TerminalChannels =
  CloneCache<Uuid, (Sender<Bytes>, CancellationToken)>;

pub fn terminal_channels() -> &'static TerminalChannels {
  static TERMINAL_CHANNELS: OnceLock<TerminalChannels> =
    OnceLock::new();
  TERMINAL_CHANNELS.get_or_init(Default::default)
}

async fn handle_terminal_message(id: Uuid, bytes: Bytes) {
  let Some((channel, _)) = terminal_channels().get(&id).await else {
    warn!("No terminal channel for {id}");
    return;
  };
  let Ok(data) = data_from_transport_bytes(bytes) else {
    warn!("Got terminal message with no data for {id}");
    return;
  };
  if let Err(e) = channel.send(data).await {
    warn!("No receiver for {id} | {e:?}");
  };
}
