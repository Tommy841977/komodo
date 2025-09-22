use std::sync::Arc;

use anyhow::anyhow;
use bytes::Bytes;
use cache::CloneCache;
use periphery_client::{PeripheryConnection, all_server_channels};
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use transport::{
  auth::{ConnectionIdentifiers, LoginFlow, PublicKeyValidator},
  bytes::id_from_transport_bytes,
  channel::BufferedReceiver,
  websocket::{
    Websocket, WebsocketMessage, WebsocketReceiver as _,
    WebsocketSender as _,
  },
};
use uuid::Uuid;

pub mod client;
pub mod server;

pub struct WebsocketHandler<'a, W> {
  pub socket: W,
  pub connection_identifiers: ConnectionIdentifiers<'a>,
  pub private_key: &'a str,
  pub expected_public_key: Option<&'a str>,
  pub write_receiver: &'a mut BufferedReceiver<Bytes>,
  pub connection: &'a PeripheryConnection,
  pub handler: &'a MessageHandler,
}

impl<W: Websocket> WebsocketHandler<'_, W> {
  async fn handle<L: LoginFlow>(self) -> anyhow::Result<()> {
    let WebsocketHandler {
      mut socket,
      connection_identifiers,
      private_key,
      expected_public_key,
      write_receiver,
      connection,
      handler,
    } = self;

    L::login(
      &mut socket,
      connection_identifiers,
      private_key,
      &PeripheryPublicKeyValidator {
        expected: expected_public_key,
      },
    )
    .await?;

    let handler_cancel = CancellationToken::new();

    connection.set_connected(true);
    connection.clear_error().await;

    let (mut ws_write, mut ws_read) = socket.split();

    let forward_writes = async {
      loop {
        let next = tokio::select! {
          next = write_receiver.recv() => next,
          _ = connection.cancel.cancelled() => break,
          _ = handler_cancel.cancelled() => break,
        };

        let message = match next {
          Some(request) => Bytes::copy_from_slice(request),
          // Sender Dropped (shouldn't happen, a reference is held on 'connection').
          None => break,
        };

        match ws_write.send(message).await {
          Ok(_) => write_receiver.clear_buffer(),
          Err(e) => {
            connection.set_error(e.into()).await;
            break;
          }
        }
      }
      // Cancel again if not already
      let _ = ws_write.close(None).await;
      handler_cancel.cancel();
    };

    let handle_reads = async {
      loop {
        let next = tokio::select! {
          next = ws_read.recv() => next,
          _ = connection.cancel.cancelled() => break,
          _ = handler_cancel.cancelled() => break,
        };

        match next {
          Ok(WebsocketMessage::Binary(bytes)) => {
            handler.handle_incoming_bytes(bytes).await
          }
          Ok(WebsocketMessage::Close(_))
          | Ok(WebsocketMessage::Closed) => {
            connection.set_error(anyhow!("Connection closed")).await;
            break;
          }
          Err(e) => {
            connection.set_error(e.into()).await;
          }
        };
      }
      // Cancel again if not already
      handler_cancel.cancel();
    };

    tokio::join!(forward_writes, handle_reads);

    connection.set_connected(false);

    Ok(())
  }
}

pub struct PeripheryPublicKeyValidator<'a> {
  /// If None, ignore public key.
  pub expected: Option<&'a str>,
}
impl PublicKeyValidator for PeripheryPublicKeyValidator<'_> {
  fn validate(&self, public_key: String) -> anyhow::Result<()> {
    if let Some(expected) = self.expected
      && public_key != expected
    {
      Err(
        anyhow!("Invalid public key '{public_key}'")
          .context("Ensure public key matches configured Periphery Public Key")
          .context("Core failed to validate Periphery public key"),
      )
    } else {
      Ok(())
    }
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
