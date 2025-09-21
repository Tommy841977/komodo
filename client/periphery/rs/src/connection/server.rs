use anyhow::Context;
use axum::{
  extract::WebSocketUpgrade,
  http::{HeaderMap, StatusCode},
  response::Response,
};
use serror::AddStatusCode;
use tracing::warn;
use transport::{
  auth::{ConnectionIdentifiers, ServerLoginFlow, compute_accept},
  websocket::axum::AxumWebsocket,
};

use crate::connection::{
  MessageHandler, PeripheryConnection, periphery_connections,
};

pub async fn handler(
  server_id: String,
  mut headers: HeaderMap,
  query: String,
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  let host = headers
    .remove("x-forwarded-host")
    .or(headers.remove("host"))
    .context("Failed to get connection host")
    .status_code(StatusCode::UNAUTHORIZED)?;
  let ws_key = headers
    .get("sec-websocket-key")
    .context("Headers do not contain Sec-Websocket-Key")
    .status_code(StatusCode::UNAUTHORIZED)?;
  let ws_accept = compute_accept(ws_key.as_bytes());

  let handler = MessageHandler::new(&server_id).await;

  let (connection, mut write_receiver) =
    PeripheryConnection::new(None);

  if let Some(existing_connection) = periphery_connections()
    .insert(server_id.clone(), connection.clone())
    .await
  {
    existing_connection.cancel();
  }

  Ok(ws.on_upgrade(|socket| async move {
    if let Err(e) = super::handle_websocket::<ServerLoginFlow>(
      AxumWebsocket(socket),
      ConnectionIdentifiers {
        host: host.as_bytes(),
        query: query.as_bytes(),
        accept: ws_accept.as_bytes(),
      },
      b"TEST",
      &mut write_receiver,
      &connection,
      &handler,
      &server_id,
    )
    .await
    {
      warn!(
        "PERIPHERY: [{server_id}] Client failed to login | {e:#}"
      );
      connection.set_error(e).await;
      return;
    }
  }))
}
