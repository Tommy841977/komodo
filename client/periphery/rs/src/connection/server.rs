use anyhow::Context;
use axum::{
  extract::WebSocketUpgrade,
  http::{HeaderMap, StatusCode},
  response::Response,
};
use bytes::Bytes;
use serror::AddStatusCode;
use tracing::{error, info, warn};
use transport::{
  auth2::{
    ConnectionIdentifiers, compute_accept, handle_server_side_login,
  },
  websocket::{
    WebsocketMessage, WebsocketReceiver, WebsocketSender,
    axum::AxumWebsocket,
  },
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
    let mut socket = AxumWebsocket(socket);

    let id = ConnectionIdentifiers {
      host: host.as_bytes(),
      query: query.as_bytes(),
      accept: ws_accept.as_bytes(),
    };

    // TODO: use proper private key
    if let Err(e) =
      handle_server_side_login(&mut socket, id, b"RANDOM_SERVER_PK")
        .await
    {
      warn!("PERIPHERY: Client failed to login | {e:#}");
      connection.set_error(e).await;
      return;
    };

    info!("PERIPHERY: {server_id} logged in");

    connection.set_connected(true);
    connection.clear_error().await;

    let (mut ws_write, mut ws_read) = socket.split();

    let forward_writes = async {
      loop {
        let next = tokio::select! {
          next = write_receiver.recv() => next,
          _ = connection.cancel.cancelled() => {
            info!("WRITE: Connection cancelled");
            break
          },
        };

        let msg = match next {
          // Sender Dropped (shouldn't happen, a reference is held on 'connection').
          None => break,
          // This has to copy the bytes to follow ownership rules.
          Some(msg) => Bytes::copy_from_slice(msg),
        };

        match ws_write.send(msg).await {
          // Clears the stored message from receiver buffer.
          // TODO: Move after response ack.
          Ok(_) => write_receiver.clear_buffer(),
          Err(e) => {
            warn!("Failed to send response | {e:?}");
            break;
          }
        }
      }
      // Cancel again if not already
      let _ = ws_write.close(None).await;
      connection.cancel();
    };

    let handle_reads = async {
      loop {
        let next = tokio::select! {
          next = ws_read.recv() => next,
          _ = connection.cancel.cancelled() => {
            info!("READ: Connection cancelled");
            break
          },
        };

        match next {
          Ok(WebsocketMessage::Binary(bytes)) => {
            handler.handle_incoming_bytes(bytes).await
          }
          Ok(WebsocketMessage::Close(frame)) => {
            warn!("Connection closed with frame: {frame:?}");
            break;
          }
          Ok(WebsocketMessage::Closed) => {
            warn!("Connection already closed");
            break;
          }
          Err(e) => {
            error!("Failed to read websocket message | {e:?}");
            break;
          }
        };
      }
      // Cancel again if not already
      connection.cancel();
    };

    tokio::join!(forward_writes, handle_reads);

    warn!("PERIPHERY: {server_id} Disconnnected");
    connection.set_connected(false);
  }))
}
