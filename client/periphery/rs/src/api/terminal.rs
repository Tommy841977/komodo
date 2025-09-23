use komodo_client::{
  api::write::TerminalRecreateMode,
  entities::{NoData, server::TerminalInfo},
};
use resolver_api::Resolve;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Execute Sentinels
pub const START_OF_OUTPUT: &str = "__KOMODO_START_OF_OUTPUT__";
pub const END_OF_OUTPUT: &str = "__KOMODO_END_OF_OUTPUT__";

#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(Vec<TerminalInfo>)]
#[error(serror::Error)]
pub struct ListTerminals {}

#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(NoData)]
#[error(serror::Error)]
pub struct CreateTerminal {
  /// The name of the terminal to create
  pub name: String,
  /// The shell command (eg `bash`) to init the shell.
  ///
  /// This can also include args:
  /// `docker exec -it container sh`
  #[serde(default = "default_command")]
  pub command: String,
  /// Default: `Never`
  #[serde(default)]
  pub recreate: TerminalRecreateMode,
}

fn default_command() -> String {
  String::from("bash")
}

//

#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(Uuid)]
#[error(serror::Error)]
pub struct ConnectTerminal {
  /// The name of the terminal to connect to
  pub terminal: String,
}

//

#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(Uuid)]
#[error(serror::Error)]
pub struct ConnectContainerExec {
  /// The name of the container to connect to.
  pub container: String,
  /// The shell to start inside container.
  /// Default: `sh`
  #[serde(default = "default_container_shell")]
  pub shell: String,
}

//

/// Used to disconnect both Terminals and Container Exec sessions.
#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(NoData)]
#[error(serror::Error)]
pub struct DisconnectTerminal {
  /// The connection id of the terminal to disconnect from
  pub id: Uuid,
}

//

#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(NoData)]
#[error(serror::Error)]
pub struct DeleteTerminal {
  /// The name of the terminal to delete
  pub terminal: String,
}

//

#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(NoData)]
#[error(serror::Error)]
pub struct DeleteAllTerminals {}

//

/// Note: The `terminal` must already exist, created by [CreateTerminal].
#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(Uuid)]
#[error(serror::Error)]
pub struct ExecuteTerminal {
  /// Specify the terminal to execute the command on.
  pub terminal: String,
  /// The command to execute.
  pub command: String,
}

//

#[derive(Serialize, Deserialize, Debug, Clone, Resolve)]
#[response(Uuid)]
#[error(serror::Error)]
pub struct ExecuteContainerExec {
  /// The name of the container to execute command in.
  pub container: String,
  /// The shell to start inside container.
  /// Default: `sh`
  #[serde(default = "default_container_shell")]
  pub shell: String,
  /// The command to execute.
  pub command: String,
}

fn default_container_shell() -> String {
  String::from("sh")
}
