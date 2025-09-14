use std::path::PathBuf;

use anyhow::Context;
use command::run_komodo_command_with_sanitization;
use environment::write_env_file;
use interpolate::Interpolator;
use komodo_client::{
  entities::{
    EnvironmentVar, RepoExecutionArgs, RepoExecutionResponse,
    SearchCombinator, SystemCommand, all_logs_success,
  },
  parsers::QUOTE_PATTERN,
};
use periphery_client::api::git::PeripheryRepoExecutionResponse;

use crate::config::periphery_config;

// ============
//  Formatting
// ============

pub fn format_extra_args(extra_args: &[String]) -> String {
  let args = extra_args.join(" ");
  if !args.is_empty() {
    format!(" {args}")
  } else {
    args
  }
}

pub fn format_labels(labels: &[EnvironmentVar]) -> String {
  labels
    .iter()
    .map(|p| {
      if p.value.starts_with(QUOTE_PATTERN)
        && p.value.ends_with(QUOTE_PATTERN)
      {
        // If the value already wrapped in quotes, don't wrap it again
        format!(" --label {}={}", p.variable, p.value)
      } else {
        format!(" --label {}=\"{}\"", p.variable, p.value)
      }
    })
    .collect::<Vec<_>>()
    .join("")
}

pub fn format_log_grep(
  terms: &[String],
  combinator: SearchCombinator,
  invert: bool,
) -> String {
  let maybe_invert = if invert { " -v" } else { Default::default() };
  match combinator {
    SearchCombinator::Or => {
      format!("grep{maybe_invert} -E '{}'", terms.join("|"))
    }
    SearchCombinator::And => {
      format!(
        "grep{maybe_invert} -P '^(?=.*{})'",
        terms.join(")(?=.*")
      )
    }
  }
}

// =====
//  Git
// =====

pub async fn handle_post_repo_execution(
  mut res: RepoExecutionResponse,
  mut environment: Vec<EnvironmentVar>,
  env_file_path: &str,
  mut on_clone: Option<SystemCommand>,
  mut on_pull: Option<SystemCommand>,
  skip_secret_interp: bool,
  mut replacers: Vec<(String, String)>,
) -> anyhow::Result<PeripheryRepoExecutionResponse> {
  if !skip_secret_interp {
    let mut interpolotor =
      Interpolator::new(None, &periphery_config().secrets);
    interpolotor.interpolate_env_vars(&mut environment)?;
    if let Some(on_clone) = on_clone.as_mut() {
      interpolotor.interpolate_string(&mut on_clone.command)?;
    }
    if let Some(on_pull) = on_pull.as_mut() {
      interpolotor.interpolate_string(&mut on_pull.command)?;
    }
    replacers.extend(interpolotor.secret_replacers);
  }

  let env_file_path = write_env_file(
    &environment,
    &res.path,
    env_file_path,
    &mut res.logs,
  )
  .await;

  let mut res = PeripheryRepoExecutionResponse { res, env_file_path };

  if let Some(on_clone) = on_clone
    && !on_clone.is_none()
  {
    let path = res
      .res
      .path
      .join(on_clone.path)
      .components()
      .collect::<PathBuf>();
    if let Some(log) = run_komodo_command_with_sanitization(
      "On Clone",
      path.as_path(),
      on_clone.command,
      true,
      &replacers,
    )
    .await
    {
      res.res.logs.push(log);
      if !all_logs_success(&res.res.logs) {
        return Ok(res);
      }
    }
  }

  if let Some(on_pull) = on_pull
    && !on_pull.is_none()
  {
    let path = res
      .res
      .path
      .join(on_pull.path)
      .components()
      .collect::<PathBuf>();
    if let Some(log) = run_komodo_command_with_sanitization(
      "On Pull",
      path.as_path(),
      on_pull.command,
      true,
      &replacers,
    )
    .await
    {
      res.res.logs.push(log);
    }
  }

  Ok(res)
}

// =======
//  Token
// =======

pub fn git_token_simple(
  domain: &str,
  account_username: &str,
) -> anyhow::Result<&'static str> {
  periphery_config()
    .git_providers
    .iter()
    .find(|provider| provider.domain == domain)
    .and_then(|provider| {
      provider.accounts.iter().find(|account| account.username == account_username).map(|account| account.token.as_str())
    })
    .with_context(|| format!("Did not find token in config for git account {account_username} | domain {domain}"))
}

pub fn git_token(
  core_token: Option<String>,
  args: &RepoExecutionArgs,
) -> anyhow::Result<Option<String>> {
  if core_token.is_some() {
    return Ok(core_token);
  }
  let Some(account) = &args.account else {
    return Ok(None);
  };
  let token = git_token_simple(&args.provider, account)?;
  Ok(Some(token.to_string()))
}

pub fn registry_token(
  domain: &str,
  account_username: &str,
) -> anyhow::Result<&'static str> {
  periphery_config()
    .docker_registries
    .iter()
    .find(|registry| registry.domain == domain)
    .and_then(|registry| {
      registry.accounts.iter().find(|account| account.username == account_username).map(|account| account.token.as_str())
    })
    .with_context(|| format!("did not find token in config for docker registry account {account_username} | domain {domain}"))
}

// =====
//  SSL
// =====

pub async fn ensure_ssl_certs() {
  let config = periphery_config();
  if !config.ssl_cert_file().is_file()
    || !config.ssl_key_file().is_file()
  {
    generate_self_signed_ssl_certs().await
  }
}

#[instrument]
async fn generate_self_signed_ssl_certs() {
  info!("Generating certs...");

  let config = periphery_config();

  let ssl_key_file = config.ssl_key_file();
  let ssl_cert_file = config.ssl_cert_file();

  // ensure cert folders exist
  if let Some(parent) = ssl_key_file.parent() {
    let _ = std::fs::create_dir_all(parent);
  }
  if let Some(parent) = ssl_cert_file.parent() {
    let _ = std::fs::create_dir_all(parent);
  }

  let key_path = ssl_key_file.display();
  let cert_path = ssl_cert_file.display();

  let command = format!(
    "openssl req -x509 -newkey rsa:4096 -keyout {key_path} -out {cert_path} -sha256 -days 3650 -nodes -subj \"/C=XX/CN=periphery\""
  );
  let log = run_command::async_run_command(&command).await;

  if log.success() {
    info!("✅ SSL Certs generated");
  } else {
    panic!(
      "🚨 Failed to generate SSL Certs | stdout: {} | stderr: {}",
      log.stdout, log.stderr
    );
  }
}
