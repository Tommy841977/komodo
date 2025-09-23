use std::{
  sync::{
    Arc,
    atomic::{self, AtomicBool},
  },
  time::Duration,
};

use anyhow::{Context, anyhow};
use bytes::Bytes;
use cache::CloneCache;
use periphery_client::api;
use resolver_api::HasResponse;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use serror::{deserialize_error_bytes, serror_into_anyhow_error};
use tokio::sync::{
  RwLock,
  mpsc::{self, Sender, error::SendError},
};
use tokio_util::sync::CancellationToken;
use tracing::warn;
use transport::{
  MessageState,
  bytes::{
    from_transport_bytes, id_from_transport_bytes, to_transport_bytes,
  },
  channel::{BufferedReceiver, buffered_channel},
  fix_ws_address,
};
use uuid::Uuid;

use crate::state::periphery_connections;

pub mod terminal;

pub type ConnectionChannels = CloneCache<Uuid, Sender<Bytes>>;

pub struct PeripheryClient {
  pub server_id: String,
  channels: Arc<ConnectionChannels>,
}

impl PeripheryClient {
  pub async fn new(
    server_id: String,
  ) -> anyhow::Result<PeripheryClient> {
    Ok(PeripheryClient {
      channels: periphery_connections()
        .get(&server_id)
        .await
        .context("Periphery not connected")?
        .channels
        .clone(),
      server_id,
    })
  }

  pub async fn new_with_spawned_client_connection<
    F: Future<Output = anyhow::Result<()>>,
  >(
    server_id: String,
    address: &str,
    // (Server id, address)
    spawn: impl FnOnce(String, String) -> F,
  ) -> anyhow::Result<PeripheryClient> {
    if address.is_empty() {
      return Err(anyhow!("Server address cannot be empty"));
    }
    spawn(server_id.clone(), fix_ws_address(address)).await?;
    PeripheryClient::new(server_id).await
  }

  #[tracing::instrument(level = "debug", skip(self))]
  pub async fn health_check(&self) -> anyhow::Result<()> {
    self.request(api::GetHealth {}).await?;
    Ok(())
  }

  #[tracing::instrument(
    name = "PeripheryRequest",
    skip(self),
    level = "debug"
  )]
  pub async fn request<T>(
    &self,
    request: T,
  ) -> anyhow::Result<T::Response>
  where
    T: std::fmt::Debug + Serialize + HasResponse,
    T::Response: DeserializeOwned,
  {
    let connection = periphery_connections()
      .get(&self.server_id)
      .await
      .with_context(|| {
        format!("No connection found for server {}", self.server_id)
      })?;

    // Polls connected 3 times before bailing
    connection.bail_if_not_connected().await?;

    let id = Uuid::new_v4();
    let (response_sender, mut response_receiever) =
      mpsc::channel(1000);
    self.channels.insert(id, response_sender).await;

    let req_type = T::req_type();
    let data = serde_json::to_vec(&json!({
      "type": req_type,
      "params": request
    }))
    .context("Failed to serialize request to bytes")?;

    if let Err(e) = connection
      .send(to_transport_bytes(data, id, MessageState::Request))
      .await
      .context("Failed to send request over channel")
    {
      // cleanup
      self.channels.remove(&id).await;
      return Err(e);
    }

    // Poll for the associated response
    loop {
      let next = tokio::select! {
        msg = response_receiever.recv() => msg,
        // Periphery will send InProgress every 5s to avoid timeout
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
          return Err(anyhow!("Response timed out"));
        }
      };

      let bytes = match next {
        Some(bytes) => bytes,
        None => {
          return Err(anyhow!(
            "Sender dropped before response was recieved"
          ));
        }
      };

      let (state, data) = match from_transport_bytes(bytes) {
        Ok((data, _, state)) if !data.is_empty() => (state, data),
        // Ignore no data cases
        Ok(_) => continue,
        Err(e) => {
          warn!(
            "Server {} | Received invalid message | {e:#}",
            self.server_id
          );
          continue;
        }
      };
      match state {
        // TODO: improve the allocation in .to_vec
        MessageState::Successful => {
          // cleanup
          self.channels.remove(&id).await;
          return serde_json::from_slice(&data)
            .context("Failed to parse successful response");
        }
        MessageState::Failed => {
          // cleanup
          self.channels.remove(&id).await;
          return Err(deserialize_error_bytes(&data));
        }
        MessageState::InProgress => continue,
        // Shouldn't be received by this receiver
        other => {
          // TODO: delete log
          warn!(
            "Server {} | Got other message over over response channel: {other:?}",
            self.server_id
          );
          continue;
        }
      }
    }
  }
}

#[derive(Default)]
pub struct PeripheryConnections(
  CloneCache<String, Arc<PeripheryConnection>>,
);

impl PeripheryConnections {
  pub async fn insert(
    &self,
    server_id: String,
    address: Option<String>,
  ) -> (Arc<PeripheryConnection>, BufferedReceiver<Bytes>) {
    let channels = if let Some(existing_connection) =
      self.0.remove(&server_id).await
    {
      existing_connection.cancel();
      // Keep the same channels so requests
      // can handle disconnects while processing.
      existing_connection.channels.clone()
    } else {
      Default::default()
    };

    let (connection, receiver) =
      PeripheryConnection::new(address, channels);

    self.0.insert(server_id, connection.clone()).await;

    (connection, receiver)
  }

  pub async fn get(
    &self,
    server_id: &String,
  ) -> Option<Arc<PeripheryConnection>> {
    self.0.get(server_id).await
  }

  /// Remove and cancel connection
  pub async fn remove(
    &self,
    server_id: &String,
  ) -> Option<Arc<PeripheryConnection>> {
    self
      .0
      .remove(server_id)
      .await
      .inspect(|connection| connection.cancel())
  }

  pub async fn get_keys(&self) -> Vec<String> {
    self.0.get_keys().await
  }
}

#[derive(Debug)]
pub struct PeripheryConnection {
  /// Specify outbound connection address.
  /// Inbound connections have this as None
  pub address: Option<String>,
  /// Whether Periphery is currently connected.
  pub connected: AtomicBool,
  /// Stores latest connection error
  pub error: RwLock<Option<serror::Serror>>,
  /// Cancel the connection
  pub cancel: CancellationToken,
  /// Send bytes to Periphery
  pub sender: Sender<Bytes>,
  /// Send bytes from Periphery to channel handlers.
  /// Must be maintained if new connection replaces old
  /// at the same server id.
  pub channels: Arc<ConnectionChannels>,
}

impl PeripheryConnection {
  pub fn new(
    address: Option<String>,
    channels: Arc<ConnectionChannels>,
  ) -> (Arc<PeripheryConnection>, BufferedReceiver<Bytes>) {
    let (sender, receiver) = buffered_channel();
    (
      PeripheryConnection {
        address,
        sender,
        channels,
        connected: AtomicBool::new(false),
        error: RwLock::new(None),
        cancel: CancellationToken::new(),
      }
      .into(),
      receiver,
    )
  }

  pub async fn handle_incoming_bytes(&self, bytes: Bytes) {
    let id = match id_from_transport_bytes(&bytes) {
      Ok(res) => res,
      Err(e) => {
        // TODO: handle better
        warn!("Failed to read id | {e:#}");
        return;
      }
    };
    let Some(channel) = self.channels.get(&id).await else {
      // TODO: handle better
      warn!("Failed to send response | No response channel found");
      return;
    };
    if let Err(e) = channel.send(bytes).await {
      // TODO: handle better
      warn!("Failed to send response | Channel failure | {e:#}");
    }
  }

  pub async fn send(
    &self,
    value: Bytes,
  ) -> Result<(), SendError<Bytes>> {
    self.sender.send(value).await
  }

  pub fn set_connected(&self, connected: bool) {
    self.connected.store(connected, atomic::Ordering::Relaxed);
  }

  pub fn connected(&self) -> bool {
    self.connected.load(atomic::Ordering::Relaxed)
  }

  /// Polls connected 3 times (1s in between) before bailing.
  pub async fn bail_if_not_connected(&self) -> anyhow::Result<()> {
    for i in 0..3 {
      if self.connected() {
        return Ok(());
      }
      if i < 2 {
        tokio::time::sleep(Duration::from_secs(1)).await;
      }
    }
    if let Some(e) = self.error().await {
      Err(serror_into_anyhow_error(e))
    } else {
      Err(anyhow!("Server is not currently connected"))
    }
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

  pub fn cancel(&self) {
    self.cancel.cancel();
  }
}
