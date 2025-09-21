use std::{
  sync::{Arc, OnceLock},
  time::Duration,
};

use anyhow::{Context, anyhow};
use bytes::Bytes;
use cache::CloneCache;
use resolver_api::HasResponse;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use serror::{deserialize_error_bytes, serror_into_anyhow_error};
use tokio::sync::mpsc::{self, Sender};
use tracing::warn;
use transport::{
  MessageState,
  bytes::{from_transport_bytes, to_transport_bytes},
  fix_ws_address,
};
use uuid::Uuid;

pub mod api;
pub mod connection;
pub mod terminal;

pub type ServerChannels = CloneCache<Uuid, Sender<Bytes>>;
// Server id => ServerChannel
pub type AllServerChannels = CloneCache<String, Arc<ServerChannels>>;

pub fn all_server_channels() -> &'static AllServerChannels {
  static CHANNELS: OnceLock<AllServerChannels> = OnceLock::new();
  CHANNELS.get_or_init(Default::default)
}

pub struct PeripheryClient {
  pub server_id: String,
  channels: Arc<ServerChannels>,
}

impl PeripheryClient {
  pub async fn new(server_id: String) -> PeripheryClient {
    PeripheryClient {
      channels: all_server_channels()
        .get_or_insert_default(&server_id)
        .await,
      server_id,
    }
  }

  pub async fn new_with_spawned_client_connection(
    server_id: String,
    address: &str,
    private_key: String,
    expected_public_key: String,
  ) -> anyhow::Result<PeripheryClient> {
    if address.is_empty() {
      return Err(anyhow!("Server address cannot be empty"));
    }
    let periphery = PeripheryClient::new(server_id.clone()).await;
    connection::client::spawn_client_connection(
      server_id,
      fix_ws_address(address),
      private_key,
      expected_public_key,
    )
    .await?;
    Ok(periphery)
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
    let connection = connection::periphery_connections()
      .get(&self.server_id)
      .await
      .with_context(|| {
        format!("No connection found for server {}", self.server_id)
      })?;

    if !connection.connected() {
      if let Some(e) = connection.error().await {
        return Err(serror_into_anyhow_error(e));
      }
      return Err(anyhow!(
        "Server {} is not currently connected",
        self.server_id
      ));
    }

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
