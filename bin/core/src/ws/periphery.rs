use axum::{
  extract::{Query, WebSocketUpgrade},
  http::HeaderMap,
  response::Response,
};
use komodo_client::entities::server::Server;
use transport::PeripheryConnectionQuery;

use crate::config::core_config;

pub async fn handler(
  Query(PeripheryConnectionQuery { server: _server }): Query<
    PeripheryConnectionQuery,
  >,
  headers: HeaderMap,
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  let server = crate::resource::get::<Server>(&_server).await?;
  let query = format!("server={}", urlencoding::encode(&_server));
  periphery_client::connection::server::handler(
    server,
    core_config().private_key.clone(),
    core_config().periphery_public_key.clone(),
    headers,
    query,
    ws,
  )
  .await
}
