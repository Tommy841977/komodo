use anyhow::Context;
use axum::{Extension, Router, middleware, routing::post};
use komodo_client::{
  api::terminal::*,
  entities::{
    deployment::Deployment, permission::PermissionLevel,
    server::Server, stack::Stack, user::User,
  },
};
use serror::Json;
use uuid::Uuid;

use crate::{
  auth::auth_request, helpers::periphery_client,
  permission::get_check_permissions, resource::get,
  state::stack_status_cache,
};

pub fn router() -> Router {
  Router::new()
    .route("/execute", post(execute_terminal))
    .route("/execute/container", post(execute_container_exec))
    .route("/execute/deployment", post(execute_deployment_exec))
    .route("/execute/stack", post(execute_stack_exec))
    .layer(middleware::from_fn(auth_request))
}

// =================
//  ExecuteTerminal
// =================

async fn execute_terminal(
  Extension(user): Extension<User>,
  Json(request): Json<ExecuteTerminalBody>,
) -> serror::Result<axum::body::Body> {
  execute_terminal_inner(Uuid::new_v4(), request, user).await
}

#[instrument(
  name = "ExecuteTerminal",
  skip(user),
  fields(
    user_id = user.id,
  )
)]
async fn execute_terminal_inner(
  req_id: Uuid,
  ExecuteTerminalBody {
    server,
    terminal,
    command,
  }: ExecuteTerminalBody,
  user: User,
) -> serror::Result<axum::body::Body> {
  info!("/terminal/execute request | user: {}", user.username);

  let server = get_check_permissions::<Server>(
    &server,
    &user,
    PermissionLevel::Read.terminal(),
  )
  .await?;

  let stream = periphery_client(&server)
    .await?
    .execute_terminal(terminal, command)
    .await
    .context("Failed to execute command on periphery")?;

  Ok(axum::body::Body::from_stream(stream))
}

// ======================
//  ExecuteContainerExec
// ======================

async fn execute_container_exec(
  Extension(user): Extension<User>,
  Json(request): Json<ExecuteContainerExecBody>,
) -> serror::Result<axum::body::Body> {
  execute_container_exec_inner(Uuid::new_v4(), request, user).await
}

#[instrument(
  name = "ExecuteContainerExec",
  skip(user),
  fields(
    user_id = user.id,
  )
)]
async fn execute_container_exec_inner(
  req_id: Uuid,
  ExecuteContainerExecBody {
    server,
    container,
    shell,
    command,
  }: ExecuteContainerExecBody,
  user: User,
) -> serror::Result<axum::body::Body> {
  info!("ExecuteContainerExec request | user: {}", user.username);

  let server = get_check_permissions::<Server>(
    &server,
    &user,
    PermissionLevel::Read.terminal(),
  )
  .await?;

  let periphery = periphery_client(&server).await?;

  let stream = periphery
    .execute_container_exec(container, shell, command)
    .await
    .context(
      "Failed to execute container exec command on periphery",
    )?;

  Ok(axum::body::Body::from_stream(stream))
}

// =======================
//  ExecuteDeploymentExec
// =======================

async fn execute_deployment_exec(
  Extension(user): Extension<User>,
  Json(request): Json<ExecuteDeploymentExecBody>,
) -> serror::Result<axum::body::Body> {
  execute_deployment_exec_inner(Uuid::new_v4(), request, user).await
}

#[instrument(
  name = "ExecuteDeploymentExec",
  skip(user),
  fields(
    user_id = user.id,
  )
)]
async fn execute_deployment_exec_inner(
  req_id: Uuid,
  ExecuteDeploymentExecBody {
    deployment,
    shell,
    command,
  }: ExecuteDeploymentExecBody,
  user: User,
) -> serror::Result<axum::body::Body> {
  info!("ExecuteDeploymentExec request | user: {}", user.username);

  let deployment = get_check_permissions::<Deployment>(
    &deployment,
    &user,
    PermissionLevel::Read.terminal(),
  )
  .await?;

  let server = get::<Server>(&deployment.config.server_id).await?;

  let periphery = periphery_client(&server).await?;

  let stream = periphery
    .execute_container_exec(deployment.name, shell, command)
    .await
    .context(
      "Failed to execute container exec command on periphery",
    )?;

  Ok(axum::body::Body::from_stream(stream))
}

// ==================
//  ExecuteStackExec
// ==================

async fn execute_stack_exec(
  Extension(user): Extension<User>,
  Json(request): Json<ExecuteStackExecBody>,
) -> serror::Result<axum::body::Body> {
  execute_stack_exec_inner(Uuid::new_v4(), request, user).await
}

#[instrument(
  name = "ExecuteStackExec",
  skip(user),
  fields(
    user_id = user.id,
  )
)]
async fn execute_stack_exec_inner(
  req_id: Uuid,
  ExecuteStackExecBody {
    stack,
    service,
    shell,
    command,
  }: ExecuteStackExecBody,
  user: User,
) -> serror::Result<axum::body::Body> {
  info!("ExecuteStackExec request | user: {}", user.username);

  let stack = get_check_permissions::<Stack>(
    &stack,
    &user,
    PermissionLevel::Read.terminal(),
  )
  .await?;

  let server = get::<Server>(&stack.config.server_id).await?;

  let container = stack_status_cache()
    .get(&stack.id)
    .await
    .context("could not get stack status")?
    .curr
    .services
    .iter()
    .find(|s| s.service == service)
    .context("could not find service")?
    .container
    .as_ref()
    .context("could not find service container")?
    .name
    .clone();

  let periphery = periphery_client(&server).await?;

  let stream = periphery
    .execute_container_exec(container, shell, command)
    .await
    .context(
      "Failed to execute container exec command on periphery",
    )?;

  Ok(axum::body::Body::from_stream(stream))
}
