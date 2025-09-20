use anyhow::{Context, anyhow};
use resolver_api::HasResponse;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use serror::{deserialize_error_bytes, serror_into_anyhow_error};
use tokio::sync::mpsc;
use tracing::{info, warn};
use transport::{
  MessageState,
  bytes::{from_transport_bytes, to_transport_bytes},
};
use uuid::Uuid;

use crate::{connection::periphery_connections, periphery_channels};

#[tracing::instrument(name = "PeripheryRequest", level = "debug")]
pub async fn request<T>(
  server_id: &String,
  request: T,
) -> anyhow::Result<T::Response>
where
  T: std::fmt::Debug + Serialize + HasResponse,
  T::Response: DeserializeOwned,
{
  let connection =
    periphery_connections().get(server_id).await.with_context(
      || format!("No connection found for server {server_id}"),
    )?;

  if !connection.connected() {
    info!("got server not connected");
    if let Some(e) = connection.error().await {
      return Err(serror_into_anyhow_error(e));
    }
    return Err(anyhow!(
      "Server {server_id} is not currently connected"
    ));
  }

  let response_channels =
    periphery_channels().get(server_id).await.with_context(|| {
      format!("No response channels found for server {server_id}")
    })?;

  let id = Uuid::new_v4();
  let (response_sender, mut response_receiever) = mpsc::channel(1000);
  response_channels.insert(id, response_sender).await;

  let req_type = T::req_type();
  let data = serde_json::to_vec(&json!({
    "type": req_type,
    "params": request
  }))
  .context("Failed to serialize request to bytes")?;

  if let Err(e) = connection
    .send(to_transport_bytes(data, id, MessageState::Request))
    .await
    .context("Failed to send request over channel")
  {
    // cleanup
    response_channels.remove(&id).await;
    return Err(e);
  }

  // Poll for the associated response
  loop {
    let bytes = match response_receiever.recv().await {
      Some(bytes) => bytes,
      None => {
        return Err(anyhow!(
          "Sender dropped before response was recieved"
        ));
      }
    };
    let (state, data) = match from_transport_bytes(bytes) {
      Ok((data, _, state)) if !data.is_empty() => (state, data),
      // TODO: Handle no data cases
      Ok(_) => continue,
      Err(e) => {
        warn!("Received invalid message | {e:#}");
        continue;
      }
    };
    match state {
      // TODO: improve the allocation in .to_vec
      MessageState::Successful => {
        // cleanup
        response_channels.remove(&id).await;
        return serde_json::from_slice(&data)
          .context("Failed to parse successful response");
      }
      MessageState::Failed => {
        // cleanup
        response_channels.remove(&id).await;
        return Err(deserialize_error_bytes(&data));
      }
      // TODO: bail if this isn't received frequent enough
      MessageState::InProgress => continue,
      // Shouldn't be received by this receiver
      other => {
        // TODO: delete log
        warn!(
          "Got other message over over response channel: {other:?}"
        );
        continue;
      }
    }
  }
}
