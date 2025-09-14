use tokio::sync::mpsc;

/// Create a buffered channel
pub fn buffered_channel(
  buffer: usize,
) -> (mpsc::Sender<Vec<u8>>, BufferedReceiver) {
  let (sender, receiver) = mpsc::channel(buffer);
  (sender, BufferedReceiver::new(receiver))
}

/// Wrapper around channel receiver to control when
/// the latest message is dropped,
/// in case it must be re-transmitted.
#[derive(Debug)]
pub struct BufferedReceiver {
  receiver: mpsc::Receiver<Vec<u8>>,
  buffer: Option<Vec<u8>>,
}

impl BufferedReceiver {
  pub fn new(receiver: mpsc::Receiver<Vec<u8>>) -> BufferedReceiver {
    BufferedReceiver {
      receiver,
      buffer: None,
    }
  }

  /// If 'next: Some(bytes)':
  ///   - Immediately returns borrow of next.
  /// Else:
  ///   - Wait for next item
  ///   - store in 'next'
  ///   - return borrow of next.
  pub async fn recv(&mut self) -> Option<&[u8]> {
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
