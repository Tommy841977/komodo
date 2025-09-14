use std::sync::OnceLock;

use cache::CloneCache;
use futures_util::StreamExt;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use transport::{BufferedReceiver, buffered_channel};

#[derive(Debug)]
struct ConnectionChannel {
  request_sender: mpsc::Sender<Vec<u8>>,
  response_receiver: broadcast::Receiver<Vec<u8>>,
  cancel: CancellationToken,
}

impl ConnectionChannel {
  fn new() -> (
    ConnectionChannel,
    BufferedReceiver,
    broadcast::Sender<Vec<u8>>,
    CancellationToken,
  ) {
    let (request_sender, request_receiver) = buffered_channel(1000);
    let (response_sender, response_receiver) =
      broadcast::channel(1000);
    let cancel = CancellationToken::new();
    (
      ConnectionChannel {
        request_sender,
        response_receiver,
        cancel: cancel.clone(),
      },
      request_receiver,
      response_sender,
      cancel,
    )
  }
}

impl Clone for ConnectionChannel {
  fn clone(&self) -> Self {
    Self {
      request_sender: self.request_sender.clone(),
      response_receiver: self.response_receiver.resubscribe(),
      cancel: self.cancel.clone(),
    }
  }
}

type ConnectionCache = CloneCache<String, ConnectionChannel>;
fn connections() -> &'static ConnectionCache {
  static CONNECTIONS: OnceLock<ConnectionCache> = OnceLock::new();
  CONNECTIONS.get_or_init(Default::default)
}

// Assumes address already wss formatted
async fn spawn_connection(address: String) -> anyhow::Result<()> {
  let (channel, request_receiver, response_sender, cancel) =
    ConnectionChannel::new();
  if let Some(existing) =
    connections().insert(address.clone(), channel).await
  {
    existing.cancel.cancel();
  }

  tokio::spawn(async move {
    // Outer connection loop
    loop {
      let socket = match crate::ws::connect_websocket(&address).await
      {
        Ok(socket) => socket,
        Err(e) => {
          // TODO: handle connect error
          return;
        }
      };
      let (mut ws_read, mut ws_write) = socket.split();

      let forward_requests = async {
        loop {
          // match request_receiver.recv().await {
          //   None => break,

          // }
        }
      };
    }
  });

  Ok(())
}
