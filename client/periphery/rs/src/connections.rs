use std::sync::OnceLock;

use cache::CloneCache;
use tokio::sync::{broadcast, mpsc};



type ConnectionCache = CloneCache<String, ()>;
// fn connections() -> &'static () {
//   static CONNECTIONS: OnceLock<CloneCache<>> = OnceLock::new();
//   CONNECTIONS.get_or_init(|| ())
// }

// Assumes address already wss formatted
fn spawn_connection(address: String) -> anyhow::Result<()> {
  tokio::spawn(async move {
    // Outer connection loop
    loop {
      let socket = match crate::ws::connect_websocket(&address).await {
        Ok(socket) => socket,
        Err(e) => {
          // TODO: handle connect error
          return;
        }
      };

    }
  });

  Ok(())
}
