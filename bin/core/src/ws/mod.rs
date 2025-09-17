use crate::{
  auth::{auth_api_key_check_enabled, auth_jwt_check_enabled},
  helpers::query::get_user,
};
use anyhow::anyhow;
use axum::{
  Router,
  extract::ws::{Message, WebSocket},
  routing::get,
};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use komodo_client::{
  entities::{server::Server, user::User},
  ws::WsLoginMessage,
};
use periphery_client::{
  PeripheryClient, api::terminal::DisconnectTerminal,
};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use transport::{
  MessageState,
  bytes::{data_from_transport_bytes, to_transport_bytes},
};
use uuid::Uuid;

mod container;
mod deployment;
mod periphery;
mod stack;
mod terminal;
mod update;

pub fn router() -> Router {
  Router::new()
    .route("/update", get(update::handler))
    .route("/terminal", get(terminal::handler))
    .route("/container/terminal", get(container::terminal))
    .route("/deployment/terminal", get(deployment::terminal))
    .route("/stack/terminal", get(stack::terminal))
}

#[instrument(level = "debug")]
async fn user_ws_login(
  mut socket: WebSocket,
) -> Option<(WebSocket, User)> {
  let login_msg = match socket.recv().await {
    Some(Ok(Message::Text(login_msg))) => {
      LoginMessage::Ok(login_msg.to_string())
    }
    Some(Ok(msg)) => {
      LoginMessage::Err(format!("invalid login message: {msg:?}"))
    }
    Some(Err(e)) => {
      LoginMessage::Err(format!("failed to get login message: {e:?}"))
    }
    None => {
      LoginMessage::Err("failed to get login message".to_string())
    }
  };
  let login_msg = match login_msg {
    LoginMessage::Ok(login_msg) => login_msg,
    LoginMessage::Err(msg) => {
      let _ = socket.send(Message::text(msg)).await;
      let _ = socket.close().await;
      return None;
    }
  };
  match WsLoginMessage::from_json_str(&login_msg) {
    // Login using a jwt
    Ok(WsLoginMessage::Jwt { jwt }) => {
      match auth_jwt_check_enabled(&jwt).await {
        Ok(user) => {
          let _ = socket.send(Message::text("LOGGED_IN")).await;
          Some((socket, user))
        }
        Err(e) => {
          let _ = socket
            .send(Message::text(format!(
              "failed to authenticate user using jwt | {e:#}"
            )))
            .await;
          let _ = socket.close().await;
          None
        }
      }
    }
    // login using api keys
    Ok(WsLoginMessage::ApiKeys { key, secret }) => {
      match auth_api_key_check_enabled(&key, &secret).await {
        Ok(user) => {
          let _ = socket.send(Message::text("LOGGED_IN")).await;
          Some((socket, user))
        }
        Err(e) => {
          let _ = socket
            .send(Message::text(format!(
              "failed to authenticate user using api keys | {e:#}"
            )))
            .await;
          let _ = socket.close().await;
          None
        }
      }
    }
    Err(e) => {
      let _ = socket
        .send(Message::text(format!(
          "failed to parse login message: {e:#}"
        )))
        .await;
      let _ = socket.close().await;
      None
    }
  }
}

enum LoginMessage {
  /// The text message
  Ok(String),
  /// The err message
  Err(String),
}

#[instrument(level = "debug")]
async fn check_user_valid(user_id: &str) -> anyhow::Result<User> {
  let user = get_user(user_id).await?;
  if !user.enabled {
    return Err(anyhow!("user not enabled"));
  }
  Ok(user)
}

async fn handle_container_terminal(
  mut client_socket: WebSocket,
  server: &Server,
  container: String,
  shell: String,
) {
  let periphery = match crate::helpers::periphery_client(server) {
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

  trace!("connecting to periphery container exec websocket");

  let (periphery_connection_id, periphery_sender, periphery_receiver) =
    match periphery.connect_container_exec(container, shell).await {
      Ok(ws) => ws,
      Err(e) => {
        debug!(
          "Failed connect to periphery container exec websocket | {e:#}"
        );
        let _ = client_socket
          .send(Message::text(format!("ERROR: {e:#}")))
          .await;
        let _ = client_socket.close().await;
        return;
      }
    };

  trace!("connected to periphery container exec websocket");

  forward_ws_channel(
    periphery,
    client_socket,
    periphery_connection_id,
    periphery_sender,
    periphery_receiver,
  )
  .await
}

async fn forward_ws_channel(
  periphery: PeripheryClient,
  client_socket: axum::extract::ws::WebSocket,
  periphery_connection_id: Uuid,
  periphery_sender: Sender<Bytes>,
  mut periphery_receiver: Receiver<Bytes>,
) {
  let (mut core_send, mut core_receive) = client_socket.split();
  let cancel = CancellationToken::new();

  trace!("starting ws exchange");

  let core_to_periphery = async {
    loop {
      let res = tokio::select! {
        res = core_receive.next() => res,
        _ = cancel.cancelled() => {
          trace!("core to periphery read: cancelled from inside");
          break;
        }
      };
      match res {
        Some(Ok(Message::Binary(data))) => {
          if let Err(e) = periphery_sender
            .send(to_transport_bytes(
              data.into(),
              periphery_connection_id,
              MessageState::Terminal,
            ))
            .await
          {
            debug!("Failed to send terminal message | {e:?}",);
            cancel.cancel();
            break;
          };
        }
        Some(Ok(Message::Text(data))) => {
          let data: Bytes = data.into();
          if let Err(e) = periphery_sender
            .send(to_transport_bytes(
              data.into(),
              periphery_connection_id,
              MessageState::Terminal,
            ))
            .await
          {
            debug!("Failed to send terminal message | {e:?}",);
            cancel.cancel();
            break;
          };
        }
        // TODO: Disconnect from periphery when client disconnects
        Some(Ok(Message::Close(_frame))) => {
          cancel.cancel();
          break;
        }
        // Ignore
        Some(Ok(_)) => {}
        Some(Err(_e)) => {
          cancel.cancel();
          break;
        }
        None => {
          cancel.cancel();
          break;
        }
      }
    }
  };

  let periphery_to_core = async {
    loop {
      let res = tokio::select! {
        res = periphery_receiver.recv() => res.map(data_from_transport_bytes),
        _ = cancel.cancelled() => {
          trace!("periphery to core read: cancelled from inside");
          break;
        }
      };
      match res {
        Some(Ok(bytes)) => {
          if let Err(e) = core_send.send(Message::Binary(bytes)).await
          {
            debug!("{e:?}");
            cancel.cancel();
            break;
          };
        }
        // No data, ignore
        Some(Err(_e)) => {}
        None => {
          let _ = core_send.send(Message::text("STREAM EOF")).await;
          cancel.cancel();
          break;
        }
      }
    }
  };

  tokio::join!(core_to_periphery, periphery_to_core);

  if let Err(e) = periphery
    .request(DisconnectTerminal {
      id: periphery_connection_id,
    })
    .await
  {
    warn!(
      "Failed to disconnect Periphery terminal forwarding | {e:#}",
    )
  }
}
