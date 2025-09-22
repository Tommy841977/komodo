use anyhow::Context;
use colored::Colorize;
use komodo_client::entities::config::cli::args::key::{
  KeyCommand, KeyOutputFormat, KeyPair,
};

pub async fn handle(command: &KeyCommand) -> anyhow::Result<()> {
  match command {
    KeyCommand::Generate { format } => {
      let keys = noise::Base64KeyPair::generate()
        .context("Failed to generate key pair")?;
      match format {
        KeyOutputFormat::Standard => {
          println!(
            "\nPublic Key: {}",
            keys.public_key.green().bold()
          );
          // Prefixed with 'base64:' so Core / Periphery know to parse
          // private key as base64.
          println!(
            "Private Key: {}{}",
            "base64:".red().bold(),
            keys.private_key.red().bold()
          );
        }
        KeyOutputFormat::Json => {
          print_json(&keys.private_key, &keys.public_key)?
        }
      }

      Ok(())
    }
    KeyCommand::Compute {
      private_key,
      format,
    } => {
      let public_key = noise::compute_public_key(private_key)
        .context("Failed to compute public key")?;
      match format {
        KeyOutputFormat::Standard => {
          println!("\nPublic Key: {}", public_key.green().bold());
        }
        KeyOutputFormat::Json => {
          print_json(private_key, &public_key)?
        }
      }
      Ok(())
    }
  }
}

fn print_json(
  private_key: &str,
  public_key: &str,
) -> anyhow::Result<()> {
  let json = serde_json::to_string_pretty(&KeyPair {
    private_key,
    public_key,
  })
  .context("Failed to serialize JSON")?;
  println!("{json}");
  Ok(())
}
