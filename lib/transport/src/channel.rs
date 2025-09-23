use std::ops::Deref;

use tokio::sync::mpsc;

const RESPONSE_BUFFER_MAX_LEN: usize = 1_024;

/// Create a buffered channel
pub fn buffered_channel<T: Deref>()
-> (mpsc::Sender<T>, BufferedReceiver<T>) {
  let (sender, receiver) = mpsc::channel(RESPONSE_BUFFER_MAX_LEN);
  (sender, BufferedReceiver::new(receiver))
}

/// Wrapper around channel receiver to control when
/// the latest message is dropped,
/// in case it must be re-transmitted.
#[derive(Debug)]
pub struct BufferedReceiver<T> {
  receiver: mpsc::Receiver<T>,
  buffer: Option<T>,
}

impl<T: Deref> BufferedReceiver<T> {
  pub fn new(receiver: mpsc::Receiver<T>) -> BufferedReceiver<T> {
    BufferedReceiver {
      receiver,
      buffer: None,
    }
  }

  /// - If 'next: Some(bytes)':
  ///   - Immediately returns borrow of next.
  /// - Else:
  ///   - Wait for next item
  ///   - store in 'next'
  ///   - return borrow of next.
  pub async fn recv(&mut self) -> Option<&<T as Deref>::Target> {
    if self.buffer.is_none() {
      self.buffer = Some(self.receiver.recv().await?);
    }
    self.buffer.as_deref()
  }

  /// Clears buffer.
  /// Should be called after transmission confirmed.
  pub fn clear_buffer(&mut self) {
    self.buffer = None;
  }
}
