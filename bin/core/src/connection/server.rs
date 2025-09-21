use anyhow::Context;
use axum::{
  extract::{Query, WebSocketUpgrade},
  http::{HeaderMap, StatusCode},
  response::Response,
};
use komodo_client::entities::server::Server;
use periphery_client::periphery_connections;
use serror::AddStatusCode;
use tracing::warn;
use transport::{
  PeripheryConnectionQuery,
  auth::{ServerHeaderIdentifiers, ServerLoginFlow},
  websocket::axum::AxumWebsocket,
};

use crate::{
  config::core_config,
  connection::{MessageHandler, PeripheryConnection},
};

pub async fn handler(
  Query(PeripheryConnectionQuery { server: _server }): Query<
    PeripheryConnectionQuery,
  >,
  mut headers: HeaderMap,
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  let identifiers = ServerHeaderIdentifiers::extract(&mut headers)
    .status_code(StatusCode::UNAUTHORIZED)?;

  let server = crate::resource::get::<Server>(&_server)
    .await
    .status_code(StatusCode::BAD_REQUEST)?;

  let expected_public_key = if server.config.public_key.is_empty() {
    core_config().periphery_public_key.clone().context("Must either configure Server 'Periphery Public Key' or set KOMODO_PERIPHERY_PUBLIC_KEY")?
  } else {
    server.config.public_key
  };

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
    let query = format!("server={}", urlencoding::encode(&_server));
    let handler = super::WebsocketHandler {
      label: &server.id,
      socket: AxumWebsocket(socket),
      connection_identifiers: identifiers.build(query.as_bytes()),
      private_key: if server.config.private_key.is_empty() {
        &core_config().private_key
      } else {
        &server.config.private_key
      },
      expected_public_key: Some(&expected_public_key),
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
    }
  }))
}
