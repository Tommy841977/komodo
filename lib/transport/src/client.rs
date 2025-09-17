use std::{
  sync::{
    Arc,
    atomic::{self, AtomicBool},
  },
  time::Duration,
};

use anyhow::Context;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use rustls::{ClientConfig, client::danger::ServerCertVerifier};
use tokio::{net::TcpStream, sync::RwLock};
use tokio_tungstenite::{
  Connector, MaybeTlsStream, WebSocketStream, tungstenite::Message,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{TransportHandler, channel::BufferedReceiver};

/// Fixes server addresses:
///   server.domain => wss://server.domain
///   http://server.domain => ws://server.domain
///   https://server.domain => wss://server.domain
pub fn fix_ws_address(address: &str) -> String {
  if address.starts_with("ws://") || address.starts_with("wss://") {
    return address.to_string();
  }
  if address.starts_with("http://") {
    return address.replace("http://", "ws://");
  }
  if address.starts_with("https://") {
    return address.replace("https://", "wss://");
  }
  format!("wss://{address}")
}

pub async fn handle_reconnecting_websocket<
  T: TransportHandler + Send + Sync + 'static,
>(
  address: &str,
  connection: &ClientConnection,
  transport: &T,
  write_receiver: &mut BufferedReceiver<Bytes>,
) {
  loop {
    let socket = match connect_websocket(address).await {
      Ok(socket) => socket,
      Err(e) => {
        connection.set_error(e).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        continue;
      }
    };

    info!("Connected to {address}");
    connection.connected.store(true, atomic::Ordering::Relaxed);
    connection.clear_error().await;

    let (mut ws_write, mut ws_read) = socket.split();

    let forward_writes = async {
      loop {
        let next = tokio::select! {
          next = write_receiver.recv() => next,
          _ = connection.cancel.cancelled() => break,
        };

        let message = match next {
          None => {
            info!("Got None over request reciever for {address}");
            break;
          }
          Some(request) => {
            Message::Binary(Bytes::copy_from_slice(request))
          }
        };

        match ws_write.send(message).await {
          Ok(_) => write_receiver.clear_buffer(),
          Err(e) => {
            warn!("Failed to send request to {address} | {e:#}");
            break;
          }
        }
      }
      // Cancel again if not already
      let _ = ws_write.close().await;
      connection.cancel();
    };

    let handle_reads = async {
      loop {
        let next = tokio::select! {
          next = ws_read.next() => next,
          _ = connection.cancel.cancelled() => break,
        };

        match next {
          Some(Ok(Message::Binary(bytes))) => {
            transport.handle_incoming_bytes(bytes).await
          }
          Some(Ok(Message::Close(frame))) => {
            warn!(
              "Connection to {address} broken with frame: {frame:?}"
            );
            break;
          }
          Some(Err(e)) => {
            warn!("Connection to {address} broken with error: {e:?}");
            break;
          }
          None => {
            warn!("Connection to {address} closed");
            break;
          }
          // Can ignore other message types
          Some(Ok(_)) => {
            continue;
          }
        };
      }
      // Cancel again if not already
      connection.cancel();
    };

    tokio::join!(forward_writes, handle_reads);

    warn!("Disconnnected from {address}");
    connection.connected.store(false, atomic::Ordering::Relaxed);
  }
}

#[derive(Debug)]
pub struct ClientConnection {
  connected: AtomicBool,
  error: RwLock<Option<serror::Serror>>,
  cancel: CancellationToken,
}

impl ClientConnection {
  pub fn new() -> ClientConnection {
    ClientConnection {
      connected: AtomicBool::new(false),
      error: RwLock::new(None),
      cancel: CancellationToken::new(),
    }
  }

  pub fn connected(&self) -> bool {
    self.connected.load(atomic::Ordering::Relaxed)
  }

  pub async fn error(&self) -> Option<serror::Serror> {
    self.error.read().await.clone()
  }

  pub async fn set_error(&self, e: anyhow::Error) {
    let mut error = self.error.write().await;
    *error = Some(e.into());
  }

  pub async fn clear_error(&self) {
    let mut error = self.error.write().await;
    *error = None;
  }

  pub fn cancel(&self) {
    self.cancel.cancel();
  }
}

pub async fn connect_websocket(
  url: &str,
) -> anyhow::Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
  let (stream, _) = if url.starts_with("wss") {
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

  Ok(stream)
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
