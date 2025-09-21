//! Implementes both sides of Noise handshake
//! using asymmetric private-public key authentication.
//!
//! TODO: Revisit
//! Note. Relies on Server being behind trusted TLS connection.
//! This is trivial for Periphery -> Core connection, but presents a challenge
//! for Core -> Periphery, where untrusted TLS certs are being used.

use anyhow::{Context, anyhow};
use axum::http::{HeaderMap, HeaderValue};
use base64::{Engine, prelude::BASE64_STANDARD};
use bytes::Bytes;
use noise::NoiseHandshake;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{MessageState, websocket::Websocket};

pub struct ConnectionIdentifiers<'a> {
  /// Server hostname
  pub host: &'a [u8],
  /// Query: 'server=<SERVER>'
  pub query: &'a [u8],
  /// Sec-Websocket-Accept, unique for each connection
  pub accept: &'a [u8],
}

pub trait PublicKeyValidator {
  fn validate(&self, public_key: String) -> anyhow::Result<()>;
}

pub trait LoginFlow {
  /// Pass base64-encoded private key
  fn login(
    socket: &mut impl Websocket,
    connection_identifiers: ConnectionIdentifiers<'_>,
    private_key: &str,
    public_key_validator: &impl PublicKeyValidator,
  ) -> impl Future<Output = anyhow::Result<()>>;
}

pub struct ServerLoginFlow;

impl LoginFlow for ServerLoginFlow {
  async fn login(
    socket: &mut impl Websocket,
    connection_identifiers: ConnectionIdentifiers<'_>,
    private_key: &str,
    public_key_validator: &impl PublicKeyValidator,
  ) -> anyhow::Result<()> {
    let res = async {
      // Server generates random nonce and sends to client
      let nonce = nonce();
      socket
        .send(Bytes::from_owner(nonce))
        .await
        .context("Failed to send connection nonce")?;

      let mut handshake = NoiseHandshake::new_responder(
        private_key,
        // Builds the handshake using the connection-unique prologue hash.
        // The prologue must be the same on both sides of connection.
        &connection_identifiers.hash(&nonce),
      )
      .context("Failed to inialize handshake")?;

      // Receive and read handshake_m1
      let handshake_m1 = socket
        .recv_bytes()
        .await
        .context("Failed to get handshake_m1")?;
      handshake
        .read_message(&handshake_m1)
        .context("Failed to read handshake_m1")?;

      // Send handshake_m2
      let handshake_m2 = handshake
        .next_message()
        .context("Failed to write handshake_m2")?;
      socket
        .send(handshake_m2)
        .await
        .context("Failed to send handshake_m2")?;

      // Receive and read handshake_m3
      let handshake_m3 = socket
        .recv_bytes()
        .await
        .context("Failed to get handshake_m3")?;
      handshake
        .read_message(&handshake_m3)
        .context("Failed to read handshake_m3")?;

      // Server now has client public key
      public_key_validator
        .validate(handshake.remote_public_key()?)
        .context("Failed to validate remote public key")?;

      anyhow::Ok(())
    }
    .await;

    match res {
      Ok(_) => {
        socket
          .send(MessageState::Successful.into())
          .await
          .context("Failed to send login successful to client")?;
        Ok(())
      }
      Err(e) => {
        if let Err(e) = socket
          .send(MessageState::Successful.into())
          .await
          .context("Failed to send login successful to client")
        {
          // Log additional error
          warn!("{e:#}");
          // Close socket
          let _ = socket.close(None).await;
        }
        // Return the original error
        Err(e)
      }
    }
  }
}

pub struct ClientLoginFlow;

impl LoginFlow for ClientLoginFlow {
  async fn login(
    socket: &mut impl Websocket,
    connection_identifiers: ConnectionIdentifiers<'_>,
    private_key: &str,
    public_key_validator: &impl PublicKeyValidator,
  ) -> anyhow::Result<()> {
    // Receive nonce from server
    let nonce = socket
      .recv_bytes()
      .await
      .context("Failed to receive connection nonce")?;

    let mut handshake = NoiseHandshake::new_initiator(
      private_key,
      // Builds the handshake using the connection-unique prologue hash.
      // The prologue must be the same on both sides of connection.
      &connection_identifiers.hash(&nonce),
    )
    .context("Failed to inialize handshake")?;

    // Send handshake_m1
    let handshake_m1 = handshake
      .next_message()
      .context("Failed to write handshake m1")?;
    socket
      .send(handshake_m1)
      .await
      .context("Failed to send handshake_m1")?;

    // Receive and read handshake_m2
    let handshake_m2 = socket
      .recv_bytes()
      .await
      .context("Failed to get handshake_m2")?;
    handshake
      .read_message(&handshake_m2)
      .context("Failed to read handshake_m2")?;

    // Client now has server public key,
    // can perform validation.
    public_key_validator
      .validate(handshake.remote_public_key()?)
      .context("Failed to validate remote public key")?;

    // Send handshake_m3
    let handshake_m3 = handshake
      .next_message()
      .context("Failed to write handshake_m3")?;
    socket
      .send(handshake_m3)
      .await
      .context("Failed to send handshake_m3")?;

    // Receive login state message and return based on value
    let state = socket
      .recv_bytes()
      .await
      .context("Failed to receive authentication state message")?;
    let state = state.first().context(
      "Authentication state message did not contain state byte",
    )?;
    match MessageState::from_byte(*state) {
      MessageState::Successful => Ok(()),
      // Todo: More descriptive error?
      _ => Err(anyhow!("Authentication failed")),
    }
  }
}

fn nonce() -> [u8; 32] {
  let mut out = [0u8; 32];
  rand::rng().fill_bytes(&mut out);
  out
}

impl ConnectionIdentifiers<'_> {
  /// nonce: Server computed random connection nonce, sent to client before auth handshake
  pub fn hash(&self, nonce: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"noise-wss-v1|");
    hash.update(self.host);
    hash.update(b"|");
    hash.update(self.query);
    hash.update(b"|");
    hash.update(self.accept);
    hash.update(b"|");
    hash.update(nonce);
    hash.finalize().into()
  }
}

/// Used to extract owned connection identifier
/// in server side connection handler.
pub struct ServerHeaderIdentifiers {
  pub host: HeaderValue,
  pub accept: String,
}

impl ServerHeaderIdentifiers {
  pub fn extract(
    headers: &mut HeaderMap,
  ) -> anyhow::Result<ServerHeaderIdentifiers> {
    let host = headers
      .remove("x-forwarded-host")
      .or(headers.remove("host"))
      .context("Failed to get connection host")?;
    let key = headers
      .remove("sec-websocket-key")
      .context("Headers do not contain Sec-Websocket-Key")?;
    let accept = compute_accept(key.as_bytes());
    Ok(ServerHeaderIdentifiers { host, accept })
  }

  pub fn build<'a>(
    &'a self,
    query: &'a [u8],
  ) -> ConnectionIdentifiers<'a> {
    ConnectionIdentifiers {
      host: self.host.as_bytes(),
      accept: self.accept.as_bytes(),
      query,
    }
  }
}

// pub fn extract_server_identifiers(headers: &mut HeaderMap) -> anyhow::Result<(Head)> {

// }

pub fn compute_accept(sec_websocket_key: &[u8]) -> String {
  // This is standard GUID to compute Sec-Websocket-Accept
  const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
  let mut sha1 = sha1::Sha1::new();
  sha1.update(sec_websocket_key);
  sha1.update(GUID);
  let digest = sha1.finalize();
  BASE64_STANDARD.encode(digest)
}
