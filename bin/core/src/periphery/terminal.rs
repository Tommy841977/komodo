use std::{
  pin::Pin,
  sync::Arc,
  task::{self, Poll},
};

use anyhow::Context;
use bytes::Bytes;
use cache::CloneCache;
use futures::Stream;
use periphery_client::api::terminal::{
  ConnectContainerExec, ConnectTerminal, END_OF_OUTPUT,
  ExecuteContainerExec, ExecuteTerminal,
};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use transport::bytes::data_from_transport_bytes;
use uuid::Uuid;

use crate::{
  periphery::PeripheryClient, state::periphery_connections,
};

impl PeripheryClient {
  pub async fn connect_terminal(
    &self,
    terminal: String,
  ) -> anyhow::Result<(Uuid, Sender<Bytes>, Receiver<Bytes>)> {
    tracing::trace!(
      "request | type: ConnectTerminal | terminal name: {terminal}",
    );

    let connection = periphery_connections()
      .get(&self.server_id)
      .await
      .with_context(|| {
        format!("No connection found for server {}", self.server_id)
      })?;

    let id = self
      .request(ConnectTerminal { terminal })
      .await
      .context("Failed to create terminal connection")?;

    let (sender, receiever) = channel(1024);
    connection.channels.insert(id, sender).await;

    Ok((id, connection.sender.clone(), receiever))
  }

  pub async fn connect_container_exec(
    &self,
    container: String,
    shell: String,
  ) -> anyhow::Result<(Uuid, Sender<Bytes>, Receiver<Bytes>)> {
    tracing::trace!(
      "request | type: ConnectContainerExec | container name: {container} | shell: {shell}",
    );

    let connection = periphery_connections()
      .get(&self.server_id)
      .await
      .with_context(|| {
        format!("No connection found for server {}", self.server_id)
      })?;

    let id = self
      .request(ConnectContainerExec { container, shell })
      .await
      .context("Failed to create container exec connection")?;

    let (sender, receiever) = channel(1000);
    connection.channels.insert(id, sender).await;

    Ok((id, connection.sender.clone(), receiever))
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

    let connection = periphery_connections()
      .get(&self.server_id)
      .await
      .with_context(|| {
        format!("No connection found for server {}", self.server_id)
      })?;

    let id = self
      .request(ExecuteTerminal { terminal, command })
      .await
      .context("Failed to create execute terminal connection")?;

    let (sender, receiver) = channel(1000);

    connection.channels.insert(id, sender).await;

    Ok(ReceiverStream {
      id,
      receiver,
      channels: connection.channels.clone(),
    })
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
  ) -> anyhow::Result<ReceiverStream> {
    tracing::trace!(
      "sending request | type: ExecuteContainerExec | container: {container} | shell: {shell} | command: {command}",
    );

    let connection = periphery_connections()
      .get(&self.server_id)
      .await
      .with_context(|| {
        format!("No connection found for server {}", self.server_id)
      })?;

    let id = self
      .request(ExecuteContainerExec {
        container,
        shell,
        command,
      })
      .await
      .context("Failed to create execute terminal connection")?;

    let (sender, receiver) = channel(1000);

    connection.channels.insert(id, sender).await;

    Ok(ReceiverStream {
      id,
      receiver,
      channels: connection.channels.clone(),
    })
  }
}

pub struct ReceiverStream {
  id: Uuid,
  channels: Arc<CloneCache<Uuid, Sender<Bytes>>>,
  receiver: Receiver<Bytes>,
}

impl Stream for ReceiverStream {
  type Item = anyhow::Result<Bytes>;
  fn poll_next(
    mut self: Pin<&mut Self>,
    cx: &mut task::Context<'_>,
  ) -> Poll<Option<Self::Item>> {
    match self
      .receiver
      .poll_recv(cx)
      .map(|bytes| bytes.map(data_from_transport_bytes))
    {
      Poll::Ready(Some(Ok(bytes))) if bytes == END_OF_OUTPUT => {
        self.cleanup();
        Poll::Ready(None)
      }
      Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(bytes))),
      Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
      Poll::Ready(None) => {
        self.cleanup();
        Poll::Ready(None)
      }
      Poll::Pending => Poll::Pending,
    }
  }
}

impl ReceiverStream {
  fn cleanup(&self) {
    // Not the prettiest but it should be fine
    let channels = self.channels.clone();
    let id = self.id;
    tokio::spawn(async move {
      channels.remove(&id).await;
    });
  }
}
