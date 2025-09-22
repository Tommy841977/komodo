use std::{
  sync::{
    Arc,
    atomic::{self, AtomicBool},
  },
  time::Duration,
};

use anyhow::{Context, anyhow};
use bytes::Bytes;
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
  bytes::{from_transport_bytes, to_transport_bytes},
  channel::{BufferedReceiver, buffered_channel},
  fix_ws_address,
};
use uuid::Uuid;

use crate::state::{
  ServerChannels, all_server_channels, periphery_connections,
};

pub mod terminal;

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
    let periphery = PeripheryClient::new(server_id.clone()).await;
    spawn(server_id, fix_ws_address(address)).await?;
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

#[derive(Debug)]
pub struct PeripheryConnection {
  // Inbound connections have this as None
  pub address: Option<String>,
  pub write_sender: Sender<Bytes>,
  pub connected: AtomicBool,
  pub error: RwLock<Option<serror::Serror>>,
  pub cancel: CancellationToken,
}

impl PeripheryConnection {
  pub fn new(
    address: Option<String>,
  ) -> (Arc<PeripheryConnection>, BufferedReceiver<Bytes>) {
    let (write_sender, write_receiver) = buffered_channel(1000);
    (
      PeripheryConnection {
        address,
        write_sender,
        connected: AtomicBool::new(false),
        error: RwLock::new(None),
        cancel: CancellationToken::new(),
      }
      .into(),
      write_receiver,
    )
  }

  pub async fn send(
    &self,
    value: Bytes,
  ) -> Result<(), SendError<Bytes>> {
    self.write_sender.send(value).await
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
