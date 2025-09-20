use std::sync::{
  Arc, OnceLock,
  atomic::{self, AtomicBool},
};

use bytes::Bytes;
use cache::CloneCache;
use tokio::sync::{
  RwLock,
  mpsc::{Sender, error::SendError},
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use transport::{
  bytes::id_from_transport_bytes,
  channel::{BufferedReceiver, buffered_channel},
};
use uuid::Uuid;

use crate::periphery_channels;

pub mod client;
pub mod server;

pub struct MessageHandler {
  channels: Arc<CloneCache<Uuid, Sender<Bytes>>>,
}

impl MessageHandler {
  pub async fn new(server_id: &String) -> MessageHandler {
    MessageHandler {
      channels: periphery_channels()
        .get_or_insert_default(server_id)
        .await,
    }
  }

  async fn handle_incoming_bytes(&self, bytes: Bytes) {
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
  // Inbound connections have this as None
  pub address: Option<String>,
  pub write_sender: Sender<Bytes>,
  pub connected: AtomicBool,
  pub error: RwLock<Option<serror::Serror>>,
  pub cancel: CancellationToken,
}

impl PeripheryConnection {
  fn new(
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
    // TODO: remove logs
    info!("Cancelling connection");
    self.cancel.cancel();
  }
}
