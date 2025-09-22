use serde::Serialize;

#[derive(Debug, Clone, clap::Subcommand)]
pub enum KeyCommand {
  /// Generate a new public / private key pair
  /// for use with Core - Periphery authentication.
  /// (aliases: `gen`, `g`)
  #[clap(alias = "gen", alias = "g")]
  Generate {
    /// Specify the format of the output.
    #[arg(long, short = 'f', default_value_t = KeyOutputFormat::Standard)]
    format: KeyOutputFormat,
  },

  /// Compute the public key for a given private key.
  /// (aliases: `comp`, `c`)
  #[clap(alias = "comp", alias = "c")]
  Compute {
    /// Pass the private key
    private_key: String,
    /// Specify the format of the output.
    #[arg(long, short = 'f', default_value_t = KeyOutputFormat::Standard)]
    format: KeyOutputFormat,
  },
}

#[derive(
  Debug, Clone, Copy, Default, strum::Display, clap::ValueEnum,
)]
#[strum(serialize_all = "lowercase")]
pub enum KeyOutputFormat {
  /// Readable output format. Default. (alias: `t`)
  #[default]
  #[clap(alias = "s")]
  Standard,
  /// Json (single line) output format. (alias: `j`)
  #[clap(alias = "j")]
  Json,
  /// Json "pretty" (multi line) output format. (alias: `jp`)
  #[clap(alias = "jp")]
  JsonPretty,
}

#[derive(Serialize)]
pub struct KeyPair<'a> {
  pub private_key: &'a str,
  pub public_key: &'a str,
}
