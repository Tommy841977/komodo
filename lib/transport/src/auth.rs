use anyhow::{Context, anyhow};
use bytes::Bytes;
use tracing::{info, warn};

use crate::{
  MessageState,
  websocket::{Websocket, WebsocketMessage},
};

#[derive(Debug, Clone, Copy)]
pub enum AuthType {
  Passkey = 0,
  Noise = 1,
}

pub async fn handle_client_side_login(
  socket: &mut impl Websocket,
  credentials: Bytes,
) -> anyhow::Result<()> {
  socket
    .send(credentials)
    .await
    .context("Failed to send login credentials")?;

  let response = match socket.recv().await? {
    WebsocketMessage::Binary(bytes) => bytes,
    WebsocketMessage::Close(frame) => {
      return Err(anyhow!(
        "Websocket close frame received during login | frame: {frame:?}"
      ));
    }
    WebsocketMessage::Closed => {
      return Err(anyhow!("Websocket closed during login"));
    }
  };
  let state = response
    .first()
    .map(|b| MessageState::from_byte(*b))
    .context("Login response is empty")?;
  if matches!(state, MessageState::Successful) {
    return Ok(());
  } else {
    return Err(anyhow!("Failed to login | Invalid credentails"));
  }
}

pub async fn handle_server_side_login(
  socket: &mut impl Websocket,
  validate_credentials: impl Fn(&[u8]) -> bool,
) -> anyhow::Result<()> {
  let credentials = match socket.recv().await? {
    WebsocketMessage::Binary(bytes) => bytes,
    WebsocketMessage::Close(frame) => {
      return Err(anyhow!(
        "Websocket close frame received during login | frame: {frame:?}"
      ));
    }
    WebsocketMessage::Closed => {
      return Err(anyhow!("Websocket closed during login"));
    }
  };
  if validate_credentials(&credentials) {
    // Send login confirmation
    // TODO: remove / edit logs
    info!("Client logged in");
    socket.send(MessageState::Successful.into()).await?;
    return Ok(());
  } else {
    // Send login failure
    warn!("Client failed to log in");
    socket.send(MessageState::Failed.into()).await?;
    let _ = socket.close(None).await;
    return Err(anyhow!("Received invalid credentials"));
  }
}
