use std::sync::OnceLock;

use resolver_api::HasResponse;
use serde::{Serialize, de::DeserializeOwned};

pub mod api;
pub mod connection;
pub mod message;

mod request;
mod terminal;
mod ws;

pub use request::request;

fn periphery_http_client() -> &'static reqwest::Client {
  static PERIPHERY_HTTP_CLIENT: OnceLock<reqwest::Client> =
    OnceLock::new();
  PERIPHERY_HTTP_CLIENT.get_or_init(|| {
    reqwest::Client::builder()
      // Use to allow communication with Periphery self-signed certs.
      .danger_accept_invalid_certs(true)
      .build()
      .expect("Failed to build Periphery http client")
  })
}
pub struct PeripheryClient {
  id: String,
  address: String,
  passkey: String,
}

impl PeripheryClient {
  pub fn new(
    id: impl Into<String>,
    address: impl Into<String>,
    passkey: impl Into<String>,
  ) -> PeripheryClient {
    PeripheryClient {
      id: id.into(),
      address: address.into(),
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
