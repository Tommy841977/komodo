use axum::{
  extract::{Query, WebSocketUpgrade, ws::Message},
  response::IntoResponse,
};
use futures::SinkExt;
use komodo_client::{
  api::terminal::ConnectTerminalQuery,
  entities::{permission::PermissionLevel, server::Server},
};

use crate::{
  helpers::periphery_client, permission::get_check_permissions,
  ws::forward_ws_channel,
};

#[instrument(name = "ConnectTerminal", skip(ws))]
pub async fn handler(
  Query(ConnectTerminalQuery { server, terminal }): Query<
    ConnectTerminalQuery,
  >,
  ws: WebSocketUpgrade,
) -> impl IntoResponse {
  ws.on_upgrade(|socket| async move {
    let Some((mut client_socket, user)) =
      super::user_ws_login(socket).await
    else {
      return;
    };

    let server = match get_check_permissions::<Server>(
      &server,
      &user,
      PermissionLevel::Read.terminal(),
    )
    .await
    {
      Ok(server) => server,
      Err(e) => {
        debug!("could not get server | {e:#}");
        let _ = client_socket
          .send(Message::text(format!("ERROR: {e:#}")))
          .await;
        let _ = client_socket.close().await;
        return;
      }
    };

    let periphery = match periphery_client(&server) {
      Ok(periphery) => periphery,
      Err(e) => {
        debug!("couldn't get periphery | {e:#}");
        let _ = client_socket
          .send(Message::text(format!("ERROR: {e:#}")))
          .await;
        let _ = client_socket.close().await;
        return;
      }
    };

    trace!("connecting to periphery terminal websocket");

    let (
      periphery_connection_id,
      periphery_sender,
      periphery_receiver,
    ) = match periphery.connect_terminal(terminal).await {
      Ok(ws) => ws,
      Err(e) => {
        debug!("Failed connect to periphery terminal | {e:#}");
        let _ = client_socket
          .send(Message::text(format!("ERROR: {e:#}")))
          .await;
        let _ = client_socket.close().await;
        return;
      }
    };

    trace!("connected to periphery terminal websocket");

    forward_ws_channel(
      periphery,
      client_socket,
      periphery_connection_id,
      periphery_sender,
      periphery_receiver,
    )
    .await
  })
}
