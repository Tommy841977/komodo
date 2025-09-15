use std::{sync::OnceLock, time::Duration};

use anyhow::{Context, anyhow};
use cache::CloneCache;
use futures_util::{SinkExt, StreamExt};
use serror::deserialize_error_bytes;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::{bytes::Bytes, sync::CancellationToken};
use tracing::warn;
use transport::channel::{BufferedReceiver, buffered_channel};
use uuid::Uuid;

use crate::message::{
  MessageState, from_transport_bytes, id_from_transport_bytes,
  to_transport_bytes,
};

#[derive(Debug)]
struct Connection {
  cancel: CancellationToken,
  request_sender: mpsc::Sender<Vec<u8>>,
}

impl Connection {
  fn new()
  -> (Connection, BufferedReceiver<Vec<u8>>, CancellationToken) {
    let (request_sender, request_receiver) = buffered_channel(1000);
    let cancel = CancellationToken::new();
    (
      Connection {
        cancel: cancel.clone(),
        request_sender,
      },
      request_receiver,
      cancel,
    )
  }
}

impl Clone for Connection {
  fn clone(&self) -> Self {
    Self {
      cancel: self.cancel.clone(),
      request_sender: self.request_sender.clone(),
    }
  }
}

/// server id => connection
type Connections = CloneCache<String, Connection>;
fn connections() -> &'static Connections {
  static CONNECTIONS: OnceLock<Connections> = OnceLock::new();
  CONNECTIONS.get_or_init(Default::default)
}

type ResponseChannels = CloneCache<Uuid, mpsc::Sender<Vec<u8>>>;
fn response_channels() -> &'static ResponseChannels {
  static RESPONSE_CHANNELS: OnceLock<ResponseChannels> =
    OnceLock::new();
  RESPONSE_CHANNELS.get_or_init(Default::default)
}

pub async fn send_request(
  server_id: &String,
  data: &[u8],
) -> anyhow::Result<Vec<u8>> {
  let request_sender = &connections()
    .get(server_id)
    .await
    .with_context(|| {
      format!("No connection found for server {server_id}")
    })?
    .request_sender;
  let id = Uuid::new_v4();
  let (response_sender, mut response_receiever) = mpsc::channel(1000);
  response_channels().insert(id, response_sender).await;
  if let Err(e) = request_sender
    .send(to_transport_bytes(id, MessageState::InProgress, data))
    .await
    .context("Failed to send request over channel")
  {
    // cleanup
    response_channels().remove(&id).await;
    return Err(e);
  }

  // Poll for the response
  loop {
    let bytes = match response_receiever.recv().await {
      Some(bytes) => bytes,
      None => {
        return Err(anyhow!(
          "Sender dropped before response was recieved"
        ));
      }
    };
    let (state, data) = match from_transport_bytes(&bytes) {
      Ok((_, state, Some(data))) => (state, data),
      // TODO: Handle no data cases
      Ok(_) => continue,
      Err(e) => {
        warn!("Received invalid message | {e:#}");
        continue;
      }
    };
    match state {
      // TODO: improve the allocation in .to_vec
      MessageState::Successful => return Ok(data.to_vec()),
      MessageState::Failed => {
        return Err(deserialize_error_bytes(&data));
      }
      // TODO: bail if this isn't received frequent enough
      MessageState::InProgress => continue,
    }
  }
}

// Assumes address already wss formatted
pub async fn spawn_connection(
  server_id: String,
  address: String,
) -> anyhow::Result<()> {
  let response_channels = response_channels();
  let (channel, mut request_receiver, cancel) = Connection::new();
  if let Some(existing) =
    connections().insert(server_id, channel).await
  {
    existing.cancel.cancel();
  }

  tokio::spawn(async move {
    let connection = async {
      // Outer connection loop
      loop {
        let socket =
          match crate::ws::connect_websocket(&address).await {
            Ok(socket) => socket,
            Err(e) => {
              warn!("{e:#}");
              tokio::time::sleep(Duration::from_secs(5)).await;
              continue;
            }
          };

        let (mut ws_write, mut ws_read) = socket.split();

        let forward_requests = async {
          loop {
            let message = match request_receiver.recv().await {
              None => break,
              Some(request) => {
                Message::Binary(Bytes::copy_from_slice(request))
              }
            };
            match ws_write.send(message).await {
              Ok(_) => request_receiver.clear_buffer(),
              Err(e) => {
                warn!("Failed to send request to {address} | {e:#}");
                break;
              }
            }
          }
        };

        let read_responses = async {
          loop {
            let bytes = match ws_read.next().await {
              Some(Ok(Message::Binary(bytes))) => bytes,
              Some(Ok(Message::Close(frame))) => {
                warn!(
                  "Connection to {address} broken with frame: {frame:?}"
                );
                break;
              }
              Some(Err(e)) => {
                warn!(
                  "Connection to {address} broken with error: {e:?}"
                );
                break;
              }
              None => {
                warn!("Connection to {address} closed");
                break;
              }
              // Can ignore other message types
              Some(Ok(_)) => {
                continue;
              }
            };
            let id = match id_from_transport_bytes(&bytes) {
              Ok(res) => res,
              Err(e) => {
                warn!("Failed to read id from {address} | {e:#}");
                continue;
              }
            };
            let Some(channel) = response_channels.get(&id).await
            else {
              warn!(
                "Failed to send response for {address} | No response channel found"
              );
              continue;
            };
            if let Err(e) = channel.send(bytes.into()).await {
              warn!(
                "Failed to send response for {address} | Channel failure | {e:#}"
              );
            }
          }
        };

        tokio::select! {
          _ = forward_requests => {},
          _ = read_responses => {}
        };
      }
    };

    tokio::select! {
      _ = connection => {},
      _ = cancel.cancelled() => {}
    }
  });

  Ok(())
}
