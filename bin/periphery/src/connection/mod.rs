use std::{sync::OnceLock, time::Duration};

use anyhow::anyhow;
use bytes::Bytes;
use cache::CloneCache;
use resolver_api::Resolve;
use response::JsonBytes;
use serror::serialize_error_bytes;
use tokio::sync::{Mutex, mpsc::Sender};
use tokio_util::sync::CancellationToken;
use transport::{
  MessageState,
  auth::{ConnectionIdentifiers, LoginFlow, PublicKeyValidator},
  bytes::{
    data_from_transport_bytes, id_state_from_transport_bytes,
    to_transport_bytes,
  },
  channel::{BufferedReceiver, buffered_channel},
  websocket::{
    Websocket, WebsocketMessage, WebsocketReceiver,
    WebsocketSender as _,
  },
};
use uuid::Uuid;

use crate::{
  api::{Args, PeripheryRequest},
  config::periphery_config,
  terminal::{ResizeDimensions, StdinMsg},
};

pub mod client;
pub mod server;

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

pub struct WebsocketHandler<'a, W, S> {
  pub socket: W,
  pub connection_identifiers: ConnectionIdentifiers<'a>,
  pub write_receiver: &'a mut BufferedReceiver<Bytes>,
  pub on_login_success: S,
}

impl<W: Websocket, S: FnMut()> WebsocketHandler<'_, W, S> {
  async fn handle<L: LoginFlow>(self) -> anyhow::Result<()> {
    let WebsocketHandler {
      mut socket,
      connection_identifiers,
      write_receiver,
      mut on_login_success,
    } = self;

    L::login(
      &mut socket,
      connection_identifiers,
      &periphery_config().private_key,
      &CorePublicKeyValidator,
    )
    .await?;
    on_login_success();

    let config = periphery_config();
    info!(
      "Logged in to Core connection websocket{}",
      if config.core_host.is_some()
        && let Some(connect_as) = &config.connect_as
      {
        format!(" as Server {connect_as}")
      } else {
        String::new()
      }
    );

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

    Ok(())
  }
}

pub struct CorePublicKeyValidator;
impl PublicKeyValidator for CorePublicKeyValidator {
  fn validate(&self, public_key: String) -> anyhow::Result<()> {
    if let Some(expected_public_key) =
      periphery_config().core_public_key.as_ref()
      && &public_key != expected_public_key
    {
      Err(
        anyhow!("Got invalid public key: {public_key}")
          .context("Ensure public key matches 'core_public_key' in periphery config (PERIPHERY_CORE_PUBLIC_KEY)")
          .context("Periphery failed to validate Core public key"),
      )
    } else {
      Ok(())
    }
  }
}

async fn handle_incoming_bytes(bytes: Bytes) {
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
      let (state, data) = match request.resolve(&Args).await {
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
  CloneCache<Uuid, (Sender<StdinMsg>, CancellationToken)>;

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
  let msg = match data.first() {
    Some(&0x00) => {
      StdinMsg::Bytes(Bytes::copy_from_slice(&data[1..]))
    }
    Some(&0xFF) => {
      if let Ok(dimensions) =
        serde_json::from_slice::<ResizeDimensions>(&data[1..])
      {
        StdinMsg::Resize(dimensions)
      } else {
        return;
      }
    }
    Some(_) => StdinMsg::Bytes(data),
    // No data
    None => return,
  };
  if let Err(e) = channel.send(msg).await {
    warn!("No receiver for {id} | {e:?}");
  };
}
