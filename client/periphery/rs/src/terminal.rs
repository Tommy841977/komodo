use std::{
  pin::Pin,
  task::{self, Poll},
};

use anyhow::Context;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use transport::bytes::data_from_transport_bytes;
use uuid::Uuid;

use crate::{
  PeripheryClient, api::terminal::*,
  connection::periphery_connections, periphery_response_channels,
};

impl PeripheryClient {
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
      .context("Failed to create terminal connection")?;

    let response_channels = periphery_response_channels()
      .get_or_insert_default(&self.id)
      .await;
    let (response_sender, response_receiever) = channel(1000);
    response_channels.insert(id, response_sender).await;

    Ok((id, connection.request_sender.clone(), response_receiever))
  }

  pub async fn connect_container_exec(
    &self,
    container: String,
    shell: String,
  ) -> anyhow::Result<(Uuid, Sender<Bytes>, Receiver<Bytes>)> {
    tracing::trace!(
      "request | type: ConnectContainerExec | container name: {container} | shell: {shell}",
    );

    let connection =
      periphery_connections().get(&self.id).await.with_context(
        || format!("No connection found for server {}", self.id),
      )?;

    let id = self
      .request(ConnectContainerExec { container, shell })
      .await
      .context("Failed to create container exec connection")?;

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
  ) -> anyhow::Result<
    impl Stream<Item = anyhow::Result<Bytes>> + 'static,
  > {
    tracing::trace!(
      "sending request | type: ExecuteTerminal | terminal name: {terminal} | command: {command}",
    );

    let id = self
      .request(ExecuteTerminal { terminal, command })
      .await
      .context("Failed to create execute terminal connection")?;

    let response_channels = periphery_response_channels()
      .get_or_insert_default(&self.id)
      .await;

    let (response_sender, response_receiever) = channel(1000);

    response_channels.insert(id, response_sender).await;

    let stream = ReceiverStream(response_receiever)
      .map(|bytes| data_from_transport_bytes(bytes));

    Ok(stream)
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
  ) -> anyhow::Result<
    impl Stream<Item = anyhow::Result<Bytes>> + 'static,
  > {
    tracing::trace!(
      "sending request | type: ExecuteContainerExec | container: {container} | shell: {shell} | command: {command}",
    );

    let id = self
      .request(ExecuteContainerExec {
        container,
        shell,
        command,
      })
      .await
      .context("Failed to create execute terminal connection")?;

    let response_channels = periphery_response_channels()
      .get_or_insert_default(&self.id)
      .await;

    let (response_sender, response_receiever) = channel(1000);

    response_channels.insert(id, response_sender).await;

    let stream = ReceiverStream(response_receiever)
      .map(|bytes| data_from_transport_bytes(bytes));

    Ok(stream)
  }
}

pub struct ReceiverStream<T>(Receiver<T>);

impl<T> Stream for ReceiverStream<T> {
  type Item = T;
  fn poll_next(
    mut self: Pin<&mut Self>,
    cx: &mut task::Context<'_>,
  ) -> Poll<Option<T>> {
    self.0.poll_recv(cx)
  }
}
