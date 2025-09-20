use ::bytes::Bytes;
use serde::{Deserialize, Serialize};

pub mod auth;
pub mod bytes;
pub mod channel;
pub mod websocket;

#[derive(Debug, Clone, Copy)]
pub enum MessageState {
  Successful = 0,
  Failed = 1,
  Terminal = 2,
  Request = 3,
  InProgress = 4,
}

impl From<MessageState> for Bytes {
  fn from(value: MessageState) -> Self {
    Bytes::from_owner([value.as_byte()])
  }
}

pub trait TransportHandler {
  fn handle_incoming_bytes(
    &self,
    bytes: Bytes,
  ) -> impl Future<Output = ()> + Send;
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PeripheryConnectionQuery {
  /// Server Id or name
  pub server: String,
}

/// - Fixes ws addresses:
///   - `server.domain` => `wss://server.domain`
///   - `http://server.domain` => `ws://server.domain`
///   - `https://server.domain` => `wss://server.domain`
pub fn fix_ws_address(address: &str) -> String {
  if address.starts_with("ws://") || address.starts_with("wss://") {
    return address.to_string();
  }
  if address.starts_with("http://") {
    return address.replace("http://", "ws://");
  }
  if address.starts_with("https://") {
    return address.replace("https://", "wss://");
  }
  format!("wss://{address}")
}
