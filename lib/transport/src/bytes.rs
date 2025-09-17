use anyhow::{Context, anyhow};
use bytes::Bytes;
use uuid::Uuid;

use crate::MessageState;

/// Serializes data + channel id + state to byte vec.
/// The last byte is the State, and the 16 before that is the Uuid.
pub fn to_transport_bytes(
  mut data: Vec<u8>,
  id: Uuid,
  state: MessageState,
) -> Bytes {
  data.extend(id.into_bytes());
  data.push(state.as_byte());
  data.into()
}

/// Deserializes channel id from
/// incoming transport bytes.
pub fn id_from_transport_bytes(bytes: &[u8]) -> anyhow::Result<Uuid> {
  let len = bytes.len();
  if len < 17 {
    return Err(anyhow!(
      "Transport bytes too short to include uuid + state"
    ));
  }
  Uuid::from_slice(&bytes[(len - 17)..(len - 1)])
    .context("Invalid Uuid bytes")
}

/// Deserializes channel id from
/// incoming transport bytes.
pub fn id_state_from_transport_bytes(
  bytes: &[u8],
) -> anyhow::Result<(Uuid, MessageState)> {
  let len = bytes.len();
  if len < 17 {
    return Err(anyhow!(
      "Transport bytes too short to include uuid + state"
    ));
  }
  let uuid = Uuid::from_slice(&bytes[(len - 17)..(len - 1)])
    .context("Invalid Uuid bytes")?;
  let state = MessageState::from_byte(bytes[len - 1]);
  Ok((uuid, state))
}

/// extracts data from incoming transport bytes,
/// consuming bytes in the process.
pub fn data_from_transport_bytes(
  bytes: Bytes,
) -> anyhow::Result<Bytes> {
  let len = bytes.len();
  if len < 17 {
    return Err(anyhow!(
      "Transport bytes too short to include uuid + state + data"
    ));
  }
  let mut res: Vec<u8> = bytes.into();
  res.drain((len - 17)..);
  Ok(res.into())
}

/// Deserializes channel id + data from
/// incoming transport bytes.
pub fn from_transport_bytes(
  bytes: Bytes,
) -> anyhow::Result<(Uuid, MessageState, Bytes)> {
  let (id, state) = id_state_from_transport_bytes(&bytes)?;
  let mut res: Vec<u8> = bytes.into();
  res.drain((res.len() - 17)..);
  Ok((id, state, res.into()))
}

impl MessageState {
  pub fn from_byte(byte: u8) -> MessageState {
    match byte {
      0 => MessageState::Request,
      1 => MessageState::Terminal,
      2 => MessageState::Successful,
      3 => MessageState::Failed,
      _ => MessageState::InProgress,
    }
  }

  pub fn as_byte(&self) -> u8 {
    match self {
      MessageState::Request => 0,
      MessageState::Terminal => 1,
      MessageState::Successful => 2,
      MessageState::Failed => 3,
      MessageState::InProgress => 4,
    }
  }
}
