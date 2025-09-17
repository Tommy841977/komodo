use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use cache::CloneCache;
use resolver_api::HasResponse;
use serde::{Serialize, de::DeserializeOwned};

pub mod api;
pub mod connection;

mod request;
mod terminal;

pub use request::request;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

// Server id => Channel sender map
pub type ResponseChannels =
  CloneCache<String, Arc<CloneCache<Uuid, Sender<Bytes>>>>;

pub fn periphery_response_channels() -> &'static ResponseChannels {
  static RESPONSE_CHANNELS: OnceLock<ResponseChannels> =
    OnceLock::new();
  RESPONSE_CHANNELS.get_or_init(Default::default)
}

pub struct PeripheryClient {
  pub id: String,
  passkey: String,
}

impl PeripheryClient {
  pub fn new(
    id: impl Into<String>,
    passkey: impl Into<String>,
  ) -> PeripheryClient {
    PeripheryClient {
      id: id.into(),
      passkey: passkey.into(),
    }
  }

  // tracing will skip self, to avoid including passkey in traces
  #[tracing::instrument(
    name = "PeripheryRequest",
    level = "debug",
    skip(self)
  )]
  pub async fn request<T>(
    &self,
    request: T,
  ) -> anyhow::Result<T::Response>
  where
    T: std::fmt::Debug + Serialize + HasResponse,
    T::Response: DeserializeOwned,
  {
    tracing::debug!("running health check");
    self.health_check().await?;
    tracing::debug!("health check passed. running inner request");
    self.request_inner(request).await
  }

  #[tracing::instrument(level = "debug", skip(self))]
  pub async fn health_check(&self) -> anyhow::Result<()> {
    self.request_inner(api::GetHealth {}).await?;
    Ok(())
  }

  #[tracing::instrument(level = "debug", skip(self))]
  async fn request_inner<T>(
    &self,
    request: T,
  ) -> anyhow::Result<T::Response>
  where
    T: std::fmt::Debug + Serialize + HasResponse,
    T::Response: DeserializeOwned,
  {
    request::request(&self.id, request).await
  }
}
