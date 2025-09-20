//! Wrappers to normalize behavior of websockets between Tungstenite and Axum,
//! as well as streamline process of handling socket messages.

use anyhow::anyhow;
use bytes::Bytes;

pub mod axum;
pub mod tungstenite;

/// Flattened websocket message possibilites
/// for easier handling.
pub enum WebsocketMessage<CloseFrame> {
  /// Standard message
  Binary(Bytes),
  /// Graceful close message
  Close(Option<CloseFrame>),
  /// Stream closed
  Closed,
}

/// Standard traits for websocket
pub trait Websocket {
  type CloseFrame: std::fmt::Debug;
  type Error: std::error::Error + Send + Sync + 'static;

  /// Looping receiver for websocket messages which only returns
  /// on significant messages.
  fn recv(
    &mut self,
  ) -> impl Future<
    Output = Result<WebsocketMessage<Self::CloseFrame>, Self::Error>,
  >;

  /// Looping receiver for websocket messages which only returns
  /// on significant messages.
  fn recv_bytes(
    &mut self,
  ) -> impl Future<Output = Result<Bytes, anyhow::Error>> {
    async {
      match self.recv().await? {
        WebsocketMessage::Binary(bytes) => Ok(bytes),
        WebsocketMessage::Close(frame) => {
          Err(anyhow!("Connection closed with framed: {frame:?}"))
        }
        WebsocketMessage::Closed => {
          Err(anyhow!("Connection already closed"))
        }
      }
    }
  }

  /// Streamlined sending on bytes
  fn send(
    &mut self,
    bytes: Bytes,
  ) -> impl Future<Output = Result<(), Self::Error>>;

  /// Send close message
  fn close(
    &mut self,
    frame: Option<Self::CloseFrame>,
  ) -> impl Future<Output = Result<(), Self::Error>>;
}

/// Traits for split websocket receiver
pub trait WebsocketReceiver {
  type CloseFrame: std::fmt::Debug;
  type Error: std::error::Error + Send + Sync + 'static;

  /// Looping receiver for websocket messages which only returns
  /// on significant messages.
  fn recv(
    &mut self,
  ) -> impl Future<
    Output = Result<WebsocketMessage<Self::CloseFrame>, Self::Error>,
  >;
}

/// Traits for split websocket receiver
pub trait WebsocketSender {
  type CloseFrame: std::fmt::Debug;
  type Error: std::error::Error + Send + Sync + 'static;

  /// Streamlined sending on bytes
  fn send(
    &mut self,
    bytes: Bytes,
  ) -> impl Future<Output = Result<(), Self::Error>>;

  /// Send close message
  fn close(
    &mut self,
    frame: Option<Self::CloseFrame>,
  ) -> impl Future<Output = Result<(), Self::Error>>;
}
