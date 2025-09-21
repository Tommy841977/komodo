use std::{collections::HashMap, sync::Arc, time::Duration};

use crate::{
  all_server_channels,
  config::core_config,
  connection::{MessageHandler, PeripheryConnection},
};
use anyhow::Context;
use axum::http::HeaderValue;
use komodo_client::entities::{optional_string, server::Server};
use periphery_client::periphery_connections;
use rustls::{ClientConfig, client::danger::ServerCertVerifier};
use tokio_tungstenite::Connector;
use tracing::{info, warn};
use transport::{
  auth::{ClientLoginFlow, ConnectionIdentifiers},
  fix_ws_address,
  websocket::tungstenite::TungsteniteWebsocket,
};

/// Managed connections to exactly those specified by specs (ServerId -> Address)
pub async fn manage_client_connections(servers: &[Server]) {
  let periphery_connections = periphery_connections();
  let periphery_channels = all_server_channels();

  let specs = servers
    .iter()
    .filter(|s| s.config.enabled)
    .map(|s| {
      (
        &s.id,
        (
          &s.name,
          &s.config.address,
          &s.config.private_key,
          &s.config.public_key,
        ),
      )
    })
    .collect::<HashMap<_, _>>();

  // Clear non specced / enabled server connections
  for (server_id, connection) in
    periphery_connections.get_entries().await
  {
    if !specs.contains_key(&server_id) {
      info!(
        "Specs do not container {server_id}, cancelling connection"
      );
      connection.cancel();
      periphery_connections.remove(&server_id).await;
      periphery_channels.remove(&server_id).await;
    }
  }

  // Apply latest connection specs
  for (
    server_id,
    (name, address, private_key, expected_public_key),
  ) in specs
  {
    let address = if address.is_empty() {
      address.to_string()
    } else {
      fix_ws_address(address)
    };
    match (
      address.is_empty(),
      periphery_connections.get(server_id).await,
    ) {
      // Periphery -> Core connections
      (true, Some(existing)) if existing.address.is_none() => {
        continue;
      }
      (true, Some(existing)) => {
        existing.cancel();
        continue;
      }
      (true, None) => continue,
      // Core -> Periphery connections
      (false, Some(existing))
        if existing
          .address
          .as_ref()
          .map(|a| a == &address)
          .unwrap_or_default() =>
      {
        // Connection OK
        continue;
      }
      // Recreate connection cases
      (false, Some(_)) => {}
      (false, None) => {}
    };
    // If reaches here, recreate the connection.
    if let Err(e) = spawn_client_connection(
      name.clone(),
      server_id.clone(),
      address,
      private_key.clone(),
      optional_string(expected_public_key),
    )
    .await
    {
      warn!(
        "Failed to spawn new connnection for {server_id} | {e:#}"
      );
    }
  }
}

// Assumes address already wss formatted
pub async fn spawn_client_connection(
  label: String,
  server_id: String,
  address: String,
  private_key: String,
  expected_public_key: Option<String>,
) -> anyhow::Result<()> {
  let url = ::url::Url::parse(&address)
    .context("Failed to parse server address")?;
  let host: Vec<u8> =
    url.host().context("url has no host")?.to_string().into();

  let handler = MessageHandler::new(&server_id).await;

  let (connection, mut write_receiver) =
    PeripheryConnection::new(address.clone().into());

  if let Some(existing_connection) = periphery_connections()
    .insert(server_id, connection.clone())
    .await
  {
    existing_connection.cancel();
  }

  let config = core_config();
  let private_key = if private_key.is_empty() {
    config.private_key.clone()
  } else {
    private_key
  };
  let expected_public_key = expected_public_key
    .or_else(|| config.periphery_public_key.clone());

  tokio::spawn(async move {
    loop {
      let ws = tokio::select! {
        _ = connection.cancel.cancelled() => {
          break
        }
        ws = connect_websocket(&address) => ws,
      };

      let (socket, accept) = match ws {
        Ok(res) => res,
        Err(e) => {
          connection.set_error(e).await;
          tokio::time::sleep(Duration::from_secs(5)).await;
          continue;
        }
      };

      let handler = super::WebsocketHandler {
        label: &label,
        socket,
        connection_identifiers: ConnectionIdentifiers {
          host: &host,
          accept: accept.as_bytes(),
          query: &[],
        },
        private_key: &private_key,
        expected_public_key: expected_public_key.as_deref(),
        write_receiver: &mut write_receiver,
        connection: &connection,
        handler: &handler,
      };

      if let Err(e) = handler.handle::<ClientLoginFlow>().await {
        if connection.cancel.is_cancelled() {
          break;
        }
        connection.set_error(e).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        continue;
      };
    }
  });

  Ok(())
}

pub async fn connect_websocket(
  url: &str,
) -> anyhow::Result<(TungsteniteWebsocket, HeaderValue)> {
  let (ws, mut response) = if url.starts_with("wss") {
    tokio_tungstenite::connect_async_tls_with_config(
      url,
      None,
      false,
      Some(Connector::Rustls(Arc::new(
        ClientConfig::builder()
          .dangerous()
          .with_custom_certificate_verifier(Arc::new(
            InsecureVerifier,
          ))
          .with_no_client_auth(),
      ))),
    )
    .await
    .with_context(|| {
      format!("failed to connect to websocket | url: {url}")
    })?
  } else {
    tokio_tungstenite::connect_async(url).await.with_context(
      || format!("failed to connect to websocket | url: {url}"),
    )?
  };

  let accept = response
    .headers_mut()
    .remove("sec-websocket-accept")
    .context("sec-websocket-accept")?;

  Ok((TungsteniteWebsocket(ws), accept))
}

#[derive(Debug)]
struct InsecureVerifier;

impl ServerCertVerifier for InsecureVerifier {
  fn verify_server_cert(
    &self,
    _end_entity: &rustls::pki_types::CertificateDer<'_>,
    _intermediates: &[rustls::pki_types::CertificateDer<'_>],
    _server_name: &rustls::pki_types::ServerName<'_>,
    _ocsp_response: &[u8],
    _now: rustls::pki_types::UnixTime,
  ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error>
  {
    Ok(rustls::client::danger::ServerCertVerified::assertion())
  }

  fn verify_tls12_signature(
    &self,
    _message: &[u8],
    _cert: &rustls::pki_types::CertificateDer<'_>,
    _dss: &rustls::DigitallySignedStruct,
  ) -> Result<
    rustls::client::danger::HandshakeSignatureValid,
    rustls::Error,
  > {
    Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
  }

  fn verify_tls13_signature(
    &self,
    _message: &[u8],
    _cert: &rustls::pki_types::CertificateDer<'_>,
    _dss: &rustls::DigitallySignedStruct,
  ) -> Result<
    rustls::client::danger::HandshakeSignatureValid,
    rustls::Error,
  > {
    Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
  }

  fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
    vec![
      rustls::SignatureScheme::RSA_PKCS1_SHA1,
      rustls::SignatureScheme::ECDSA_SHA1_Legacy,
      rustls::SignatureScheme::RSA_PKCS1_SHA256,
      rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
      rustls::SignatureScheme::RSA_PKCS1_SHA384,
      rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
      rustls::SignatureScheme::RSA_PKCS1_SHA512,
      rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
      rustls::SignatureScheme::RSA_PSS_SHA256,
      rustls::SignatureScheme::RSA_PSS_SHA384,
      rustls::SignatureScheme::RSA_PSS_SHA512,
      rustls::SignatureScheme::ED25519,
      rustls::SignatureScheme::ED448,
    ]
  }
}
