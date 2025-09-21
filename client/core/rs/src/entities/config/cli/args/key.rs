#[derive(Debug, Clone, clap::Subcommand)]
pub enum KeyCommand {
  /// Generate a new public / private key pair
  /// for use with Core - Periphery authentication.
  /// (aliases: `gen`, `g`)
  #[clap(alias = "gen", alias = "g")]
  Generate,

  /// Compute the public key for a given private key.
  /// (aliases: `comp`, `c`)
  #[clap(alias = "comp", alias = "c")]
  Compute {
    /// Pass the private key
    private_key: String,
  },
}
