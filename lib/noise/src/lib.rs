use anyhow::{Context, anyhow};
use base64::{engine::Engine, prelude::BASE64_STANDARD};
use bytes::Bytes;

const NOISE_XX_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Wrapper around [snow::HandshakeState] to streamline this implementation
pub struct NoiseHandshake(snow::HandshakeState);

impl NoiseHandshake {
  /// Should pass base64 encoded private key.
  pub fn new_initiator(
    private_key: &str,
    prologue: &[u8],
  ) -> anyhow::Result<NoiseHandshake> {
    if private_key.len() < 4 {
      return Err(anyhow!(
        "The private key should be at least 4 characters"
      ));
    }
    Ok(NoiseHandshake(
      snow::Builder::new(NOISE_XX_PARAMS.parse()?)
        .local_private_key(
          &BASE64_STANDARD
            .decode(private_key)
            .context("Failed to decode base64 string")?,
        )
        .context("Invalid private key")?
        .prologue(prologue)
        .context("Invalid prologue")?
        .build_initiator()
        .context("Failed to build initiator")?,
    ))
  }

  /// Should pass base64 encoded private key.
  pub fn new_responder(
    private_key: &str,
    prologue: &[u8],
  ) -> anyhow::Result<NoiseHandshake> {
    if private_key.len() < 4 {
      return Err(anyhow!(
        "The private key should be at least 4 characters"
      ));
    }
    Ok(NoiseHandshake(
      snow::Builder::new(NOISE_XX_PARAMS.parse()?)
        .local_private_key(
          &BASE64_STANDARD
            .decode(private_key)
            .context("Failed to decode base64 string")?,
        )
        .context("Invalid private key")?
        .prologue(prologue)
        .context("Invalid prologue")?
        .build_responder()
        .context("Failed to build responder")?,
    ))
  }

  /// Reads message from other side of handshake
  pub fn read_message(
    &mut self,
    message: &[u8],
  ) -> Result<(), snow::Error> {
    self.0.read_message(message, &mut []).map(|_| ())
  }

  /// Produces next message to be read on other side of handshake
  pub fn next_message(&mut self) -> Result<Bytes, snow::Error> {
    let mut buf = [0u8; 1024];
    let written = self.0.write_message(&[], &mut buf)?;
    Ok(Bytes::copy_from_slice(&buf[..written]))
  }

  /// Gets the base64-encoded remote public key.
  /// Note that this should only be called after m2 is read on client side,
  /// or m3 is read on server side.
  pub fn remote_public_key(&self) -> anyhow::Result<String> {
    let public_key = self
      .0
      .get_remote_static()
      .context("Failed to get remote public key")?;
    Ok(BASE64_STANDARD.encode(public_key))
  }
}

pub struct Base64KeyPair {
  pub private_key: String,
  pub public_key: String,
}

impl Base64KeyPair {
  pub fn generate() -> anyhow::Result<Base64KeyPair> {
    let builder = snow::Builder::new(NOISE_XX_PARAMS.parse()?);
    let kp = builder.generate_keypair()?;
    Ok(Base64KeyPair {
      private_key: BASE64_STANDARD.encode(kp.private),
      public_key: BASE64_STANDARD.encode(kp.public),
    })
  }
}

/// This completes an example handshake
/// to produce the base64-encoded public key
/// for a given base64-encoded private key.
pub fn compute_public_key(
  private_key: &str,
) -> anyhow::Result<String> {
  // Create mock client handshake. The private key doesn't matter.
  let mut client_handshake =
    NoiseHandshake::new_initiator("0000", &[])
      .context("Failed to create client handshake")?;
  // Create mock server handshake.
  // Use the target private key with server handshake,
  // since its public key is the first available in the flow.
  let mut server_handshake =
    NoiseHandshake::new_responder(private_key, &[])
      .context("Failed to create server handshake")?;
  // write message 1
  let message_1 = client_handshake
    .next_message()
    .context("CLIENT: failed to write message 1")?;
  // read message 1
  server_handshake
    .read_message(&message_1)
    .context("SERVER: failed to read message 1")?;
  // write message 2
  let message_2 = server_handshake
    .next_message()
    .context("SERVER: failed to write message 2")?;
  // read message 2
  client_handshake
    .read_message(&message_2)
    .context("CLIENT: failed to read message 2")?;
  // client now has server public key
  client_handshake
    .remote_public_key()
    .context("Failed to get public key")
}
