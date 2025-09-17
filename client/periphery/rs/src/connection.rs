use std::{
  collections::HashMap,
  sync::{Arc, OnceLock},
};

use bytes::Bytes;
use cache::CloneCache;
use komodo_client::entities::server::Server;
use tokio::sync::mpsc::{self, Sender};
use tracing::warn;
use transport::{
  TransportHandler,
  bytes::id_from_transport_bytes,
  channel::{BufferedReceiver, buffered_channel},
  client::{ClientConnection, fix_ws_address},
};
use uuid::Uuid;

// Server id => Channel sender map
pub type ResponseChannels =
  CloneCache<String, Arc<CloneCache<Uuid, mpsc::Sender<Bytes>>>>;
pub fn periphery_response_channels() -> &'static ResponseChannels {
  static RESPONSE_CHANNELS: OnceLock<ResponseChannels> =
    OnceLock::new();
  RESPONSE_CHANNELS.get_or_init(Default::default)
}

pub struct CoreTransportHandler {
  response_channels: Arc<CloneCache<Uuid, Sender<Bytes>>>,
}

impl CoreTransportHandler {
  pub async fn new(server_id: &String) -> CoreTransportHandler {
    CoreTransportHandler {
      response_channels: periphery_response_channels()
        .get_or_insert_default(server_id)
        .await,
    }
  }
}

impl TransportHandler for CoreTransportHandler {
  async fn handle_incoming_bytes(&self, bytes: Bytes) {
    let id = match id_from_transport_bytes(&bytes) {
      Ok(res) => res,
      Err(e) => {
        // TODO: handle better
        warn!("Failed to read id | {e:#}");
        return;
      }
    };
    let Some(channel) = self.response_channels.get(&id).await else {
      // TODO: handle better
      warn!("Failed to send response | No response channel found");
      return;
    };
    if let Err(e) = channel.send(bytes).await {
      // TODO: handle better
      warn!("Failed to send response | Channel failure | {e:#}");
    }
  }
}

/// Managed connections to exactly those specified by specs (ServerId -> Address)
pub async fn manage_outbound_connections(servers: &[Server]) {
  let periphery_connections = periphery_connections();
  let periphery_response_channels = periphery_response_channels();

  let specs = servers
    .iter()
    .filter(|s| !s.config.address.is_empty())
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
    let address = fix_ws_address(address);
    match periphery_connections.get(server_id).await {
      // Existing connection good to go, nothing to do
      Some(existing) if existing.address == address => {}
      // All other cases re-spawn connection
      _ => {
        if let Err(e) =
          spawn_outbound_connection(server_id.clone(), address).await
        {
          warn!(
            "Failed to spawn new connnection for {server_id} | {e:#}"
          );
        }
      }
    }
  }
}

// Assumes address already wss formatted
async fn spawn_outbound_connection(
  server_id: String,
  address: String,
) -> anyhow::Result<()> {
  let transport = CoreTransportHandler::new(&server_id).await;

  let (connection, mut request_receiver) =
    PeripheryConnection::new(address.clone());
  if let Some(existing_connection) = periphery_connections()
    .insert(server_id, connection.clone())
    .await
  {
    existing_connection.client.cancel();
  }

  tokio::spawn(async move {
    transport::client::handle_reconnecting_websocket(
      &address,
      &connection.client,
      &transport,
      &mut request_receiver,
    )
    .await
  });

  Ok(())
}

/// server id => connection
pub type PeripheryConnections =
  CloneCache<String, Arc<PeripheryConnection>>;
pub fn periphery_connections() -> &'static PeripheryConnections {
  static CONNECTIONS: OnceLock<PeripheryConnections> =
    OnceLock::new();
  CONNECTIONS.get_or_init(Default::default)
}

#[derive(Debug)]
pub struct PeripheryConnection {
  address: String,
  pub request_sender: mpsc::Sender<Bytes>,
  pub client: ClientConnection,
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
