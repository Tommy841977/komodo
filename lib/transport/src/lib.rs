use ::bytes::Bytes;

pub mod bytes;
pub mod channel;
pub mod client;
pub mod server;

#[derive(Debug, Clone, Copy)]
pub enum MessageState {
  Successful,
  Failed,
  Request,
  InProgress,
  Terminal,
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
