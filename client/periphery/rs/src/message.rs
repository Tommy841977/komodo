use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

/// Serializes channel id + data to byte vec.
/// The first 16 bytes are the Uuid, followed by the json serialized data bytes.
pub fn to_transport_bytes(
  id: Uuid,
  state: MessageState,
  data: &[u8],
) -> Vec<u8> {
  // Index 0..15
  let mut res = id.into_bytes().to_vec();
  // Index 16
  res.push(state.as_byte());
  // Index 17..end
  res.extend_from_slice(data);

  res
}

/// Deserializes channel id from
/// incoming transport bytes.
pub fn id_from_transport_bytes(bytes: &[u8]) -> anyhow::Result<Uuid> {
  if bytes.len() < 16 {
    return Err(anyhow!("Transport bytes too short to include uuid"));
  }
  Uuid::from_slice(&bytes[..16]).context("Invalid Uuid bytes")
}

/// Deserializes channel id + data from
/// incoming transport bytes.
pub fn from_transport_bytes(
  bytes: &[u8],
) -> anyhow::Result<(Uuid, MessageState, Option<&[u8]>)> {
  if bytes.len() < 17 {
    return Err(anyhow!(
      "Transport bytes too short to include uuid and state"
    ));
  }
  let (id, state, data) = (&bytes[..16], bytes[16], bytes.get(17..));
  let id = Uuid::from_slice(id).context("Invalid Uuid bytes")?;
  let state = MessageState::from_byte(state);
  Ok((id, state, data))
}

/// Deserializes channel id + data from
/// incoming transport bytes.
pub fn parse_from_transport_bytes<T: DeserializeOwned>(
  bytes: &[u8],
) -> anyhow::Result<(Uuid, MessageState, Option<T>)> {
  let (id, state, data) = from_transport_bytes(bytes)?;
  let data = data
    .map(|data| {
      serde_json::from_slice::<T>(data)
        .context("Failed to deserialize message data")
    })
    .transpose()?;
  Ok((id, state, data))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MessageState {
  Successful,
  Failed,
  InProgress,
}

impl MessageState {
  /// - 0 => Successful
  /// - 1 => Failed
  /// - other => InProgress
  pub fn from_byte(byte: u8) -> MessageState {
    match byte {
      0 => MessageState::Successful,
      1 => MessageState::Failed,
      _ => MessageState::InProgress,
    }
  }

  pub fn as_byte(&self) -> u8 {
    match self {
      MessageState::Successful => 0,
      MessageState::Failed => 1,
      MessageState::InProgress => 2,
    }
  }
}
