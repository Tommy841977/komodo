use anyhow::{Context, anyhow};
use bytes::Bytes;
use futures_util::{SinkExt, TryStreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
  MaybeTlsStream, WebSocketStream, tungstenite,
};
use tracing::{info, warn};

use crate::MessageState;

pub async fn handle_client_side_login(
  socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
  credentials: Bytes,
) -> anyhow::Result<()> {
  socket
    .send(tungstenite::Message::Binary(credentials))
    .await
    .context("Failed to send login credentials")?;

  loop {
    let response = socket
      .try_next()
      .await
      .context("Failed to receive login response")?
      .context("Stream broken before login response received")?;
    let bytes = match &response {
      tungstenite::Message::Text(text) => text.as_bytes(),
      tungstenite::Message::Binary(bytes) => &bytes,
      tungstenite::Message::Close(frame) => {
        return Err(anyhow!(
          "Websocket close frame received during login | frame: {frame:?}"
        ));
      }
      // Ignore others
      _ => continue,
    };
    let state = bytes
      .first()
      .map(|b| MessageState::from_byte(*b))
      .context("Login response is empty")?;
    if matches!(state, MessageState::Successful) {
      return Ok(());
    } else {
      return Err(anyhow!("Failed to login | Invalid credentails"));
    }
  }
}

pub async fn handle_server_side_login(
  socket: &mut axum::extract::ws::WebSocket,
  validate_credentials: impl Fn(&[u8]) -> bool,
) -> anyhow::Result<()> {
  loop {
    // Poll for next message
    let msg = socket
      .try_next()
      .await
      .context("Failed to receive login credentials")?
      .context("Stream broken before login credentials received")?;
    // Treat first message as credentials
    let credentials = match &msg {
      axum::extract::ws::Message::Text(text) => text.as_bytes(),
      axum::extract::ws::Message::Binary(bytes) => &bytes,
      axum::extract::ws::Message::Close(frame) => {
        return Err(anyhow!(
          "Websocket close frame received during login | frame: {frame:?}"
        ));
      }
      // Ignore others
      _ => continue,
    };
    // Validate
    if validate_credentials(credentials) {
      // Send login confirmation
      // TODO: remove / edit logs
      info!("Client logged in");
      socket
        .send(axum::extract::ws::Message::Binary(
          MessageState::Successful.into(),
        ))
        .await?;
      return Ok(());
    } else {
      // Send login failure
      warn!("Client failed to log in");
      socket
        .send(axum::extract::ws::Message::Binary(
          MessageState::Failed.into(),
        ))
        .await?;
      let _ = socket.close().await;
      return Err(anyhow!("Received invalid credentials"));
    }
  }
}
