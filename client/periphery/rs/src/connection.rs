use std::{
  collections::HashMap,
  sync::{
    Arc, OnceLock,
    atomic::{self, AtomicBool},
  },
  time::Duration,
};

use cache::CloneCache;
use futures_util::{SinkExt, StreamExt};
use komodo_client::entities::server::Server;
use tokio::sync::{RwLock, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::{bytes::Bytes, sync::CancellationToken};
use tracing::{info, warn};
use transport::channel::{BufferedReceiver, buffered_channel};
use uuid::Uuid;

use crate::message::id_from_transport_bytes;

/// server id => connection
pub type PeripheryConnections =
  CloneCache<String, Arc<PeripheryConnection>>;
pub fn periphery_connections() -> &'static PeripheryConnections {
  static CONNECTIONS: OnceLock<PeripheryConnections> =
    OnceLock::new();
  CONNECTIONS.get_or_init(Default::default)
}

pub type ResponseChannels =
  CloneCache<String, Arc<CloneCache<Uuid, mpsc::Sender<Vec<u8>>>>>;
pub fn periphery_response_channels() -> &'static ResponseChannels {
  static RESPONSE_CHANNELS: OnceLock<ResponseChannels> =
    OnceLock::new();
  RESPONSE_CHANNELS.get_or_init(Default::default)
}

/// Managed connections to exactly those specified by specs (ServerId -> Address)
pub async fn manage_connections(servers: &[Server]) {
  let periphery_connections = periphery_connections();
  let periphery_response_channels = periphery_response_channels();

  let specs = servers
    .iter()
    .map(|s| (&s.id, &s.config.address))
    .collect::<HashMap<_, _>>();

  // Clear non specced server connections / channels
  for (server_id, connection) in
    periphery_connections.get_entries().await
  {
    if !specs.contains_key(&server_id) {
      connection.cancel.cancel();
      periphery_connections.remove(&server_id).await;
      periphery_response_channels.remove(&server_id).await;
    }
  }

  // Apply latest connection specs
  for (server_id, address) in specs {
    let address = fix_address(address);
    match periphery_connections.get(server_id).await {
      // Existing connection good to go, nothing to do
      Some(existing) if existing.address == address => {}
      // All other cases re-spawn connection
      _ => {
        if let Err(e) =
          spawn_connection(server_id.clone(), address).await
        {
          warn!(
            "Failed to spawn new connnection for {server_id} | {e:#}"
          );
        }
      }
    }
  }
}

/// Fixes server addresses:
///   server.domain => wss://server.domain
///   http://server.domain => ws://server.domain
///   https://server.domain => wss://server.domain
fn fix_address(address: &str) -> String {
  if address.starts_with("ws://") || address.starts_with("wss://") {
    return address.to_string();
  }
  if address.starts_with("http://") {
    return address.replace("http://", "ws://");
  }
  if address.starts_with("https://") {
    return address.replace("https://", "wss://");
  }
  format!("wss://{address}")
}

// Assumes address already wss formatted
async fn spawn_connection(
  server_id: String,
  address: String,
) -> anyhow::Result<()> {
  let response_channels = periphery_response_channels()
    .get_or_insert_default(&server_id)
    .await;

  let (connection, mut request_receiver, cancel) =
    PeripheryConnection::new(address.clone());
  if let Some(existing_connection) = periphery_connections()
    .insert(server_id, connection.clone())
    .await
  {
    existing_connection.cancel.cancel();
  }

  tokio::spawn(async move {
    let connection = async {
      // Outer connection loop
      loop {
        let socket =
          match crate::ws::connect_websocket(&address).await {
            Ok(socket) => socket,
            Err(e) => {
              connection.set_error(e).await;
              tokio::time::sleep(Duration::from_secs(5)).await;
              continue;
            }
          };

        info!("Connected to {address}");
        connection.connected.store(true, atomic::Ordering::Relaxed);
        connection.clear_error().await;

        let (mut ws_write, mut ws_read) = socket.split();

        let forward_requests = async {
          loop {
            let message = match request_receiver.recv().await {
              None => {
                info!("Got None over request reciever for {address}");
                break;
              }
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

        warn!("Disconnnected from {address}");
        connection.connected.store(false, atomic::Ordering::Relaxed);
      }
    };

    tokio::select! {
      _ = connection => {},
      _ = cancel.cancelled() => {}
    }
  });

  Ok(())
}

#[derive(Debug)]
pub struct PeripheryConnection {
  address: String,
  connected: AtomicBool,
  error: RwLock<Option<serror::Serror>>,
  request_sender: mpsc::Sender<Vec<u8>>,
  cancel: CancellationToken,
}

impl PeripheryConnection {
  fn new(
    address: String,
  ) -> (
    Arc<PeripheryConnection>,
    BufferedReceiver<Vec<u8>>,
    CancellationToken,
  ) {
    let (request_sender, request_receiver) = buffered_channel(1000);
    let cancel = CancellationToken::new();
    (
      PeripheryConnection {
        address,
        connected: AtomicBool::new(false),
        error: Default::default(),
        cancel: cancel.clone(),
        request_sender,
      }
      .into(),
      request_receiver,
      cancel,
    )
  }

  pub async fn send(
    &self,
    value: Vec<u8>,
  ) -> Result<(), mpsc::error::SendError<Vec<u8>>> {
    self.request_sender.send(value).await
  }

  pub fn connected(&self) -> bool {
    self.connected.load(atomic::Ordering::Relaxed)
  }

  pub async fn error(&self) -> Option<serror::Serror> {
    self.error.read().await.clone()
  }

  pub async fn set_error(&self, e: anyhow::Error) {
    let mut error = self.error.write().await;
    *error = Some(e.into());
  }

  pub async fn clear_error(&self) {
    let mut error = self.error.write().await;
    *error = None;
  }
}
