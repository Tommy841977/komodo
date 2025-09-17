use anyhow::Context;
use bytes::Bytes;
use komodo_client::terminal::TerminalStreamResponse;
use reqwest::RequestBuilder;
use tokio::{
  net::TcpStream,
  sync::mpsc::{Receiver, Sender, channel},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use crate::{
  PeripheryClient,
  api::terminal::*,
  connection::{periphery_connections, periphery_response_channels},
};

impl PeripheryClient {
  /// Handles ws connect and login.
  /// Does not handle reconnect.
  pub async fn connect_terminal(
    &self,
    terminal: String,
  ) -> anyhow::Result<(Uuid, Sender<Bytes>, Receiver<Bytes>)> {
    tracing::trace!(
      "request | type: ConnectTerminal | terminal name: {terminal}",
    );

    let connection =
      periphery_connections().get(&self.id).await.with_context(
        || format!("No connection found for server {}", self.id),
      )?;

    let id = self
      .request(ConnectTerminal { terminal })
      .await
      .context("Failed to create terminal connectionn")?;

    let response_channels = periphery_response_channels()
      .get_or_insert_default(&self.id)
      .await;
    let (response_sender, response_receiever) = channel(1000);
    response_channels.insert(id, response_sender).await;

    Ok((id, connection.request_sender.clone(), response_receiever))
  }

  /// Executes command on specified terminal,
  /// and streams the response ending in [KOMODO_EXIT_CODE][komodo_client::entities::KOMODO_EXIT_CODE]
  /// sentinal value as the expected final line of the stream.
  ///
  /// Example final line:
  /// ```text
  /// __KOMODO_EXIT_CODE:0
  /// ```
  ///
  /// This means the command exited with code 0 (success).
  ///
  /// If this value is NOT the final item before stream closes, it means
  /// the terminal exited mid command, before giving status. Example: running `exit`.
  #[tracing::instrument(level = "debug", skip(self))]
  pub async fn execute_terminal(
    &self,
    terminal: String,
    command: String,
  ) -> anyhow::Result<TerminalStreamResponse> {
    tracing::trace!(
      "sending request | type: ExecuteTerminal | terminal name: {terminal} | command: {command}",
    );
    let req = crate::periphery_http_client()
      .post(format!("{}/terminal/execute", self.address))
      .json(&ExecuteTerminalBody { terminal, command })
      .header("authorization", &self.passkey);
    terminal_stream_response(req).await
  }

  /// Handles ws connect and login.
  /// Does not handle reconnect.
  pub async fn connect_container_exec(
    &self,
    container: String,
    shell: String,
  ) -> anyhow::Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    tracing::trace!(
      "request | type: ConnectContainerExec | container name: {container} | shell: {shell}",
    );

    let token = self
      .request(CreateTerminalAuthToken {})
      .await
      .context("Failed to create terminal auth token")?;

    let query_str = serde_qs::to_string(&ConnectContainerExecQuery {
      token: token.token,
      container,
      shell,
    })
    .context("Failed to serialize query string")?;

    let url = format!(
      "{}/terminal/container?{query_str}",
      self.address.replacen("http", "ws", 1)
    );

    transport::client::connect_websocket(&url).await
  }

  /// Executes command on specified container,
  /// and streams the response ending in [KOMODO_EXIT_CODE][komodo_client::entities::KOMODO_EXIT_CODE]
  /// sentinal value as the expected final line of the stream.
  ///
  /// Example final line:
  /// ```text
  /// __KOMODO_EXIT_CODE:0
  /// ```
  ///
  /// This means the command exited with code 0 (success).
  ///
  /// If this value is NOT the final item before stream closes, it means
  /// the container shell exited mid command, before giving status. Example: running `exit`.
  #[tracing::instrument(level = "debug", skip(self))]
  pub async fn execute_container_exec(
    &self,
    container: String,
    shell: String,
    command: String,
  ) -> anyhow::Result<TerminalStreamResponse> {
    tracing::trace!(
      "sending request | type: ExecuteContainerExec | container: {container} | shell: {shell} | command: {command}",
    );
    let req = crate::periphery_http_client()
      .post(format!("{}/terminal/execute/container", self.address))
      .json(&ExecuteContainerExecBody {
        container,
        shell,
        command,
      })
      .header("authorization", &self.passkey);
    terminal_stream_response(req).await
  }
}

async fn terminal_stream_response(
  req: RequestBuilder,
) -> anyhow::Result<TerminalStreamResponse> {
  let res =
    req.send().await.context("Failed at request to periphery")?;
  let status = res.status();
  tracing::debug!(
    "got response | type: ExecuteTerminal | {status} | response: {res:?}",
  );
  if status.is_success() {
    Ok(TerminalStreamResponse(res))
  } else {
    tracing::debug!("response is non-200");

    let text = res
      .text()
      .await
      .context("Failed to convert response to text")?;

    tracing::debug!("got response text, deserializing error");

    let error = serror::deserialize_error(text).context(status);

    Err(error)
  }
}
