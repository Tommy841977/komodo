use axum::{
  extract::WebSocketUpgrade,
  http::{HeaderMap, StatusCode},
  response::Response,
};
use komodo_client::entities::server::Server;
use serror::AddStatusCode;
use tracing::warn;
use transport::{
  auth::{ServerHeaderIdentifiers, ServerLoginFlow},
  websocket::axum::AxumWebsocket,
};

use crate::connection::{
  MessageHandler, PeripheryConnection, periphery_connections,
};

pub async fn handler(
  server: Server,
  default_private_key: String,
  mut headers: HeaderMap,
  query: String,
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  let identifiers = ServerHeaderIdentifiers::extract(&mut headers)
    .status_code(StatusCode::UNAUTHORIZED)?;

  let handler = MessageHandler::new(&server.id).await;

  let (connection, mut write_receiver) =
    PeripheryConnection::new(None);

  if let Some(existing_connection) = periphery_connections()
    .insert(server.id.clone(), connection.clone())
    .await
  {
    existing_connection.cancel();
  }

  Ok(ws.on_upgrade(|socket| async move {
    let handler = super::WebsocketHandler {
      label: &server.id,
      socket: AxumWebsocket(socket),
      connection_identifiers: identifiers.build(query.as_bytes()),
      private_key: if server.config.private_key.is_empty() {
        &default_private_key
      } else {
        &server.config.private_key
      },
      expected_public_key: &server.config.public_key,
      write_receiver: &mut write_receiver,
      connection: &connection,
      handler: &handler,
    };

    if let Err(e) = handler.handle::<ServerLoginFlow>().await {
      warn!(
        "Server {} | Client failed to login | {e:#}",
        server.name
      );
      connection.set_error(e).await;
      return;
    }
  }))
}
