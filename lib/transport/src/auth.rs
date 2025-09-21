//! Implementes both sides of Noise handshake
//! using asymmetric private-public key authentication.
//!
//! TODO: Revisit
//! Note. Relies on Server being behind trusted TLS connection.
//! This is trivial for Periphery -> Core connection, but presents a challenge
//! for Core -> Periphery, where untrusted TLS certs are being used.

use anyhow::{Context, anyhow};
use base64::{Engine, prelude::BASE64_STANDARD};
use bytes::Bytes;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{MessageState, websocket::Websocket};

const NOISE_XX_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

pub struct ConnectionIdentifiers<'a> {
  /// Server hostname
  pub host: &'a [u8],
  /// Query: 'server=<SERVER>'
  pub query: &'a [u8],
  /// Sec-Websocket-Accept, unique for each connection
  pub accept: &'a [u8],
}

pub trait LoginFlow {
  fn login(
    socket: &mut impl Websocket,
    connection_identifiers: ConnectionIdentifiers<'_>,
    private_key: &[u8],
  ) -> impl Future<Output = anyhow::Result<()>>;
}

pub struct ServerLoginFlow;

impl LoginFlow for ServerLoginFlow {
  async fn login(
    socket: &mut impl Websocket,
    connection_identifiers: ConnectionIdentifiers<'_>,
    private_key: &[u8],
  ) -> anyhow::Result<()> {
    let res = async {
      // Server generates random nonce and sends to client
      let nonce = nonce();
      socket
        .send(Bytes::from_owner(nonce))
        .await
        .context("Failed to send connection nonce")?;

      // Build the handshake using the connection-unique prologue hash.
      // The prologue must be the same on both sides of connection.
      let mut handshake =
        snow::Builder::new(NOISE_XX_PARAMS.parse()?)
          .local_private_key(private_key)?
          .prologue(&connection_identifiers.hash(&nonce))?
          .build_responder()?;

      // Receive and read handshake_m1
      let handshake_m1 = socket
        .recv_bytes()
        .await
        .context("Failed to get handshake_m1")?;
      handshake
        .read_message(&handshake_m1, &mut [])
        .context("Failed to read handshake_m1")?;

      // Send handshake_m2
      let mut handshake_m2 = [0u8; 1024];
      let written = handshake
        .write_message(&[], &mut handshake_m2)
        .context("Failed to write handshake_m2")?;
      socket
        .send(Bytes::copy_from_slice(&handshake_m2[..written]))
        .await
        .context("Failed to send handshake_m2")?;

      // Receive and read handshake_m3
      let handshake_m3 = socket
        .recv_bytes()
        .await
        .context("Failed to get handshake_m3")?;
      handshake
        .read_message(&handshake_m3, &mut [])
        .context("Failed to read handshake_m3")?;

      // Server now has client public key
      let client_public_key = handshake
        .get_remote_static()
        .context("Failed to get remote public key")?;
      println!(
        "Server got client public key: {}",
        BASE64_STANDARD.encode(client_public_key)
      );

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
    private_key: &[u8],
  ) -> anyhow::Result<()> {
    // Receive nonce from server
    let nonce = socket
      .recv_bytes()
      .await
      .context("Failed to receive connection nonce")?;

    // Build the handshake using the connection-unique prologue hash.
    // The prologue must be the same on both sides of connection.
    let mut handshake = snow::Builder::new(NOISE_XX_PARAMS.parse()?)
      .local_private_key(private_key)?
      .prologue(&connection_identifiers.hash(&nonce))?
      .build_initiator()?;

    // Send handshake_m1
    let mut handshake_m1 = [0u8; 1024];
    let written = handshake
      .write_message(&[], &mut handshake_m1)
      .context("Failed to write handshake_m1")?;
    socket
      .send(Bytes::copy_from_slice(&handshake_m1[..written]))
      .await
      .context("Failed to send handshake_m1")?;

    // Receive and read handshake_m2
    let handshake_m2 = socket
      .recv_bytes()
      .await
      .context("Failed to get handshake_m2")?;
    handshake
      .read_message(&handshake_m2, &mut [])
      .context("Failed to read handshake_m2")?;

    // Client now has server public key
    let server_public_key = handshake
      .get_remote_static()
      .context("Failed to get remote public key")?;
    println!(
      "Client got server public key: {}",
      BASE64_STANDARD.encode(server_public_key)
    );

    // Send handshake_m3
    let mut handshake_m3 = [0u8; 1024];
    let written = handshake
      .write_message(&[], &mut handshake_m3)
      .context("Failed to write handshake_m3")?;
    socket
      .send(Bytes::copy_from_slice(&handshake_m3[..written]))
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

pub fn compute_accept(sec_websocket_key: &[u8]) -> String {
  // This is standard GUID to compute Sec-Websocket-Accept
  const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
  let mut sha1 = sha1::Sha1::new();
  sha1.update(sec_websocket_key);
  sha1.update(GUID);
  let digest = sha1.finalize();
  BASE64_STANDARD.encode(digest)
}

/// This completes an example handshake
/// to produce the correct public key
/// for a given private key.
pub fn generage_public_key(
  private_key: &[u8],
) -> anyhow::Result<Bytes> {
  let mut ch = snow::Builder::new(NOISE_XX_PARAMS.parse()?)
    .local_private_key(b"ANY")?
    .build_initiator()?;
  // Use the target private key with server handshake,
  // since its public key is the first available in the flow.
  let mut sh = snow::Builder::new(NOISE_XX_PARAMS.parse()?)
    .local_private_key(private_key)?
    .build_responder()?;
  // write m1
  let mut m1 = [0u8; 1024];
  let written = ch
    .write_message(&[], &mut m1)
    .context("CLIENT: failed to write m1")?;
  // read m1
  sh.read_message(&m1[..written], &mut [])
    .context("SERVER: failed to read m1")?;
  // write m2
  let mut m2 = [0u8; 1024];
  let written = sh
    .write_message(&[], &mut m2)
    .context("SERVER: failed to write m2")?;
  // read m2
  ch.read_message(&m2[..written], &mut [])
    .context("CLIENT: failed to read m2")?;
  // client now has server public key
  Ok(Bytes::copy_from_slice(
    ch.get_remote_static().context("Failed to get public key")?,
  ))
}
