use anyhow::Context;
use colored::Colorize;
use komodo_client::entities::config::cli::args::key::KeyCommand;

pub async fn handle(command: &KeyCommand) -> anyhow::Result<()> {
  match command {
    KeyCommand::Generate => {
      let keys = noise::Base64KeyPair::generate()
        .context("Failed to generate key pair")?;
      println!("\nPublic Key: {}", keys.public_key.green().bold());
      println!(
        "Private Key: {}{}",
        "base64:".red().bold(),
        keys.private_key.red().bold()
      );
      Ok(())
    }
    KeyCommand::Compute { private_key } => {
      let public_key = noise::compute_public_key(private_key)
        .context("Failed to compute public key")?;
      println!("\nPublic Key: {}", public_key.green().bold());
      Ok(())
    }
  }
}
