pub mod bytes;
pub mod channel;
pub mod client;
pub mod server;

#[derive(Debug, Clone, Copy)]
pub enum MessageState {
  Successful,
  Failed,
  InProgress,
}
