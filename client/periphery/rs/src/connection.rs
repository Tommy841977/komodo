use std::{
  collections::HashMap,
  sync::{Arc, OnceLock},
};

use bytes::Bytes;
use cache::CloneCache;
use komodo_client::entities::server::Server;
use tokio::sync::mpsc;
use tracing::warn;
use transport::{
  channel::{BufferedReceiver, buffered_channel},
  client::ClientConnection,
};
use uuid::Uuid;

/// server id => connection
pub type PeripheryConnections =
  CloneCache<String, Arc<PeripheryConnection>>;
pub fn periphery_connections() -> &'static PeripheryConnections {
  static CONNECTIONS: OnceLock<PeripheryConnections> =
    OnceLock::new();
  CONNECTIONS.get_or_init(Default::default)
}

pub type ResponseChannels =
  CloneCache<String, Arc<CloneCache<Uuid, mpsc::Sender<Bytes>>>>;
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
      connection.client.cancel();
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

  let (connection, request_receiver) =
    PeripheryConnection::new(address.clone());
  if let Some(existing_connection) = periphery_connections()
    .insert(server_id, connection.clone())
    .await
  {
    existing_connection.client.cancel();
  }

  transport::client::spawn_reconnecting_websocket(
    address,
    connection.client.clone(),
    request_receiver,
    response_channels,
  );

  Ok(())
}

#[derive(Debug)]
pub struct PeripheryConnection {
  address: String,
  request_sender: mpsc::Sender<Bytes>,
  pub client: Arc<ClientConnection>,
}

impl PeripheryConnection {
  fn new(
    address: String,
  ) -> (Arc<PeripheryConnection>, BufferedReceiver<Bytes>) {
    let (request_sender, request_receiver) = buffered_channel(1000);
    (
      PeripheryConnection {
        address,
        request_sender,
        client: ClientConnection::new().into(),
      }
      .into(),
      request_receiver,
    )
  }

  pub async fn send(
    &self,
    value: Bytes,
  ) -> Result<(), mpsc::error::SendError<Bytes>> {
    self.request_sender.send(value).await
  }

  pub fn connected(&self) -> bool {
    self.client.connected()
  }

  pub async fn error(&self) -> Option<serror::Serror> {
    self.client.error().await
  }

  pub async fn set_error(&self, e: anyhow::Error) {
    self.client.set_error(e).await
  }

  pub async fn clear_error(&self) {
    self.client.clear_error().await
  }
}
