use std::sync::Arc;

use anyhow::{Context, anyhow};
use axum::http::StatusCode;
use bytes::Bytes;
use futures::{Stream, StreamExt, TryStreamExt};
use komodo_client::{
  api::write::TerminalRecreateMode,
  entities::{KOMODO_EXIT_CODE, NoData, server::TerminalInfo},
};
use periphery_client::api::terminal::*;
use resolver_api::Resolve;
use serror::AddStatusCodeError;
use tokio::sync::mpsc::channel;
use tokio_util::{codec::LinesCodecError, sync::CancellationToken};
use transport::{MessageState, bytes::to_transport_bytes};
use uuid::Uuid;

use crate::{
  config::periphery_config,
  connection::{terminal_channels, ws_sender},
  terminal::*,
};

//

impl Resolve<super::Args> for ListTerminals {
  #[instrument(name = "ListTerminals", level = "debug")]
  async fn resolve(
    self,
    _: &super::Args,
  ) -> serror::Result<Vec<TerminalInfo>> {
    clean_up_terminals().await;
    Ok(list_terminals().await)
  }
}

//

impl Resolve<super::Args> for CreateTerminal {
  #[instrument(name = "CreateTerminal", level = "debug")]
  async fn resolve(self, _: &super::Args) -> serror::Result<NoData> {
    if periphery_config().disable_terminals {
      return Err(
        anyhow!("Terminals are disabled in the periphery config")
          .status_code(StatusCode::FORBIDDEN),
      );
    }
    create_terminal(self.name, self.command, self.recreate)
      .await
      .map(|_| NoData {})
      .map_err(Into::into)
  }
}

//

impl Resolve<super::Args> for DeleteTerminal {
  #[instrument(name = "DeleteTerminal", level = "debug")]
  async fn resolve(self, _: &super::Args) -> serror::Result<NoData> {
    delete_terminal(&self.terminal).await;
    Ok(NoData {})
  }
}

//

impl Resolve<super::Args> for DeleteAllTerminals {
  #[instrument(name = "DeleteAllTerminals", level = "debug")]
  async fn resolve(self, _: &super::Args) -> serror::Result<NoData> {
    delete_all_terminals().await;
    Ok(NoData {})
  }
}

//

impl Resolve<super::Args> for ConnectTerminal {
  #[instrument(name = "ConnectTerminal", level = "debug")]
  async fn resolve(self, _: &super::Args) -> serror::Result<Uuid> {
    if periphery_config().disable_terminals {
      return Err(
        anyhow!("Terminals are disabled in the periphery config")
          .status_code(StatusCode::FORBIDDEN),
      );
    }

    clean_up_terminals().await;
    let terminal = get_terminal(&self.terminal).await?;

    let id = Uuid::new_v4();

    tokio::spawn(handle_terminal_forwarding(id, terminal));

    Ok(id)
  }
}

//

impl Resolve<super::Args> for ConnectContainerExec {
  #[instrument(name = "ConnectContainerExec", level = "debug")]
  async fn resolve(self, args: &super::Args) -> serror::Result<Uuid> {
    if periphery_config().disable_container_exec {
      return Err(
        anyhow!("Container exec is disabled in the periphery config")
          .into(),
      );
    }

    let ConnectContainerExec { container, shell } = self;

    if container.contains("&&") || shell.contains("&&") {
      return Err(
        anyhow!(
          "The use of '&&' is forbidden in the container name or shell"
        )
        .into(),
      );
    }
    // Create (recreate if shell changed)
    let terminal = create_terminal(
      container.clone(),
      format!("docker exec -it {container} {shell}"),
      TerminalRecreateMode::DifferentCommand,
    )
    .await
    .context("Failed to create terminal for container exec")?;

    let id = Uuid::new_v4();

    tokio::spawn(handle_terminal_forwarding(id, terminal));

    Ok(id)
  }
}

//

impl Resolve<super::Args> for DisconnectTerminal {
  #[instrument(name = "DisconnectTerminal", level = "debug")]
  async fn resolve(self, _: &super::Args) -> serror::Result<NoData> {
    // TODO: Remove logs
    info!("Disconnect called for {}", self.id);
    if let Some((_, cancel)) =
      terminal_channels().remove(&self.id).await
    {
      cancel.cancel();
      info!("Cancel called for {}", self.id);
    }
    Ok(NoData {})
  }
}

//

impl Resolve<super::Args> for ExecuteTerminal {
  #[instrument(name = "ExecuteTerminal", level = "debug")]
  async fn resolve(self, _: &super::Args) -> serror::Result<Uuid> {
    if periphery_config().disable_terminals {
      return Err(
        anyhow!("Terminals are disabled in the periphery config")
          .status_code(StatusCode::FORBIDDEN),
      );
    }

    let terminal = get_terminal(&self.terminal).await?;

    let stdout =
      setup_execute_command_on_terminal(&terminal, &self.command)
        .await?;

    let id = Uuid::new_v4();

    tokio::spawn(forward_execute_command_on_terminal_response(
      id, stdout,
    ));

    Ok(id)
  }
}

//

impl Resolve<super::Args> for ExecuteContainerExec {
  #[instrument(name = "ExecuteContainerExec", level = "debug")]
  async fn resolve(self, _: &super::Args) -> serror::Result<Uuid> {
    if periphery_config().disable_container_exec {
      return Err(
        anyhow!("Container exec is disabled in the periphery config")
          .into(),
      );
    }

    let Self {
      container,
      shell,
      command,
    } = self;

    if container.contains("&&") || shell.contains("&&") {
      return Err(
        anyhow!(
          "The use of '&&' is forbidden in the container name or shell"
        )
        .into(),
      );
    }

    // Create terminal (recreate if shell changed)
    let terminal = create_terminal(
      container.clone(),
      format!("docker exec -it {container} {shell}"),
      TerminalRecreateMode::DifferentCommand,
    )
    .await
    .context("Failed to create terminal for container exec")?;

    let stdout =
      setup_execute_command_on_terminal(&terminal, &command).await?;

    let id = Uuid::new_v4();

    tokio::spawn(forward_execute_command_on_terminal_response(
      id, stdout,
    ));

    Ok(id)
  }
}

async fn handle_terminal_forwarding(
  id: Uuid,
  terminal: Arc<Terminal>,
) {
  let ws_sender = ws_sender();
  let (sender, mut ws_receiver) = channel(1000);
  let cancel = CancellationToken::new();

  terminal_channels()
    .insert(id, (sender, cancel.clone()))
    .await;

  let init_res = async {
    let (a, b) = terminal.history.bytes_parts();
    if !a.is_empty() {
      ws_sender
        .send(to_transport_bytes(
          a.into(),
          id,
          MessageState::Terminal,
        ))
        .await
        .context("Failed to send history part a")?;
    }
    if !b.is_empty() {
      ws_sender
        .send(to_transport_bytes(
          b.into(),
          id,
          MessageState::Terminal,
        ))
        .await
        .context("Failed to send history part b")?;
    }
    anyhow::Ok(())
  }
  .await;

  if let Err(e) = init_res {
    // TODO: Handle error
    warn!("Failed to init terminal | {e:#}");
    terminal_channels().remove(&id).await;
    return;
  }

  let ws_read = async {
    loop {
      let res = tokio::select! {
        res = ws_receiver.recv() => res,
        _ = terminal.cancel.cancelled() => {
          trace!("ws read: cancelled from outside");
          break
        },
        _ = cancel.cancelled() => {
          trace!("ws read: cancelled from inside");
          break;
        }
      };
      match res {
        Some(bytes) if bytes.first() == Some(&0x00) => {
          // println!("Got ws read bytes - for stdin");
          if let Err(e) = terminal
            .stdin
            .send(StdinMsg::Bytes(Bytes::copy_from_slice(
              &bytes[1..],
            )))
            .await
          {
            debug!("WS -> PTY channel send error: {e:}");
            terminal.cancel();
            break;
          };
        }
        Some(bytes) if bytes.first() == Some(&0xFF) => {
          // println!("Got ws read bytes - for resize");
          if let Ok(dimensions) =
            serde_json::from_slice::<ResizeDimensions>(&bytes[1..])
            && let Err(e) =
              terminal.stdin.send(StdinMsg::Resize(dimensions)).await
          {
            debug!("WS -> PTY channel send error: {e:}");
            terminal.cancel();
            break;
          }
        }
        Some(bytes) => {
          trace!("Got ws read text");
          if let Err(e) =
            terminal.stdin.send(StdinMsg::Bytes(bytes)).await
          {
            debug!("WS -> PTY channel send error: {e:?}");
            terminal.cancel();
            break;
          };
        }
        None => {
          debug!("Got ws read none");
          cancel.cancel();
          break;
        }
      }
    }
  };

  let ws_write = async {
    let mut stdout = terminal.stdout.resubscribe();
    loop {
      let res = tokio::select! {
        res = stdout.recv() => res.context("Failed to get message over stdout receiver"),
        _ = terminal.cancel.cancelled() => {
          trace!("ws write: cancelled from outside");
          // let _ = ws_sender.send("PTY KILLED")).await;
          // if let Err(e) = ws_write.close().await {
          //   debug!("Failed to close ws: {e:?}");
          // };
          break
        },
        _ = cancel.cancelled() => {
          // let _ = ws_write.send(Message::Text(Utf8Bytes::from_static("WS KILLED"))).await;
          // if let Err(e) = ws_write.close().await {
          //   debug!("Failed to close ws: {e:?}");
          // };
          break
        }
      };
      match res {
        Ok(bytes) => {
          if let Err(e) = ws_sender
            .send(to_transport_bytes(
              bytes.into(),
              id,
              MessageState::Terminal,
            ))
            .await
          {
            debug!("Failed to send to WS: {e:?}");
            cancel.cancel();
            break;
          }
        }
        Err(e) => {
          debug!("PTY -> WS channel read error: {e:?}");
          let _ = ws_sender
            .send(to_transport_bytes(
              format!("ERROR: {e:#}").into(),
              id,
              MessageState::Terminal,
            ))
            .await;
          terminal.cancel();
          break;
        }
      }
    }
  };

  tokio::join!(ws_read, ws_write);

  // Clean up
  terminal_channels().remove(&id).await;
  clean_up_terminals().await;
}

/// This is run before spawning task handler
async fn setup_execute_command_on_terminal(
  terminal: &Terminal,
  command: &str,
) -> serror::Result<
  impl Stream<Item = Result<String, LinesCodecError>> + 'static,
> {
  // Read the bytes into lines
  // This is done to check the lines for the EOF sentinal
  let mut stdout = tokio_util::codec::FramedRead::new(
    tokio_util::io::StreamReader::new(
      tokio_stream::wrappers::BroadcastStream::new(
        terminal.stdout.resubscribe(),
      )
      .map(|res| res.map_err(std::io::Error::other)),
    ),
    tokio_util::codec::LinesCodec::new(),
  );

  let full_command = format!(
    "printf '\n{START_OF_OUTPUT}\n\n'; {command}; rc=$?; printf '\n{KOMODO_EXIT_CODE}%d\n{END_OF_OUTPUT}\n' \"$rc\"\n"
  );

  terminal
    .stdin
    .send(StdinMsg::Bytes(Bytes::from(full_command)))
    .await
    .context("Failed to send command to terminal stdin")?;

  // Only start the response AFTER the start sentinel is printed
  loop {
    match stdout
      .try_next()
      .await
      .context("Failed to read stdout line")?
    {
      Some(line) if line == START_OF_OUTPUT => break,
      // Keep looping until the start sentinel received.
      Some(_) => {}
      None => {
        return Err(
          anyhow!(
            "Stdout stream terminated before start sentinel received"
          )
          .into(),
        );
      }
    }
  }

  Ok(stdout)
}

async fn forward_execute_command_on_terminal_response(
  id: Uuid,
  mut stdout: impl Stream<Item = Result<String, LinesCodecError>> + Unpin,
) {
  let ws_sender = ws_sender();
  loop {
    match stdout.next().await {
      Some(Ok(line)) if line.as_str() == END_OF_OUTPUT => break,
      Some(Ok(line)) => {
        if let Err(e) = ws_sender
          .send(to_transport_bytes(
            (line + "\n").into(),
            id,
            MessageState::Terminal,
          ))
          .await
        {
          warn!("Got ws_sender send error | {e:?}");
          break;
        }
      }
      Some(Err(e)) => {
        warn!("Got stdout stream error | {e:?}");
        break;
      }
      None => {
        clean_up_terminals().await;
        break;
        // return Err(
        //   anyhow!(
        //     "Stdout stream terminated before start sentinel received"
        //   )
        //   .into(),
        // );
      }
    }
  }
}
