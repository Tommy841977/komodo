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
  auth::{ConnectionIdentifiers, LoginFlow, PublicKeyValidator},
  bytes::id_from_transport_bytes,
  channel::{BufferedReceiver, buffered_channel},
  websocket::{
    Websocket, WebsocketMessage, WebsocketReceiver as _,
    WebsocketSender as _,
  },
};
use uuid::Uuid;

use crate::all_server_channels;

pub mod client;
pub mod server;

async fn handle_websocket<L: LoginFlow>(
  mut socket: impl Websocket,
  connection_identifiers: ConnectionIdentifiers<'_>,
  private_key: &str,
  write_receiver: &mut BufferedReceiver<Bytes>,
  connection: &PeripheryConnection,
  handler: &MessageHandler,
  name: &str,
) -> anyhow::Result<()> {
  L::login(
    &mut socket,
    connection_identifiers,
    private_key,
    &PeripheryPublicKeyValidator,
  )
  .await?;

  info!("PERIPHERY: Logged into {name}");

  connection.set_connected(true);
  connection.clear_error().await;

  let (mut ws_write, mut ws_read) = socket.split();

  let forward_writes = async {
    loop {
      let next = tokio::select! {
        next = write_receiver.recv() => next,
        _ = connection.cancel.cancelled() => break,
      };

      let message = match next {
        Some(request) => Bytes::copy_from_slice(request),
        // Sender Dropped (shouldn't happen, a reference is held on 'connection').
        None => break,
      };

      match ws_write.send(message).await {
        Ok(_) => write_receiver.clear_buffer(),
        Err(e) => {
          warn!("Failed to send request to {name} | {e:#}");
          break;
        }
      }
    }
    // Cancel again if not already
    let _ = ws_write.close(None).await;
    connection.cancel();
  };

  let handle_reads = async {
    loop {
      let next = tokio::select! {
        next = ws_read.recv() => next,
        _ = connection.cancel.cancelled() => break,
      };

      match next {
        Ok(WebsocketMessage::Binary(bytes)) => {
          handler.handle_incoming_bytes(bytes).await
        }
        Ok(WebsocketMessage::Close(frame)) => {
          warn!("Connection to {name} broken with frame: {frame:?}");
          break;
        }
        Ok(WebsocketMessage::Closed) => {}
        Err(e) => {
          warn!("Connection to {name} broken with error: {e:?}");
          break;
        }
      };
    }
    // Cancel again if not already
    connection.cancel();
  };

  tokio::join!(forward_writes, handle_reads);

  warn!("PERIPHERY: Disconnnected from {name}");
  connection.set_connected(false);

  Ok(())
}

pub struct PeripheryPublicKeyValidator;
impl PublicKeyValidator for PeripheryPublicKeyValidator {
  fn validate(&self, public_key: String) -> anyhow::Result<()> {
    Ok(())
  }
}

pub struct MessageHandler {
  channels: Arc<CloneCache<Uuid, Sender<Bytes>>>,
}

impl MessageHandler {
  pub async fn new(server_id: &String) -> MessageHandler {
    MessageHandler {
      channels: all_server_channels()
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
