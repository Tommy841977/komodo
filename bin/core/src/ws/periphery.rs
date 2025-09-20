use axum::{
  extract::{Query, WebSocketUpgrade},
  http::HeaderMap,
  response::Response,
};
use komodo_client::entities::server::Server;
use transport::PeripheryConnectionQuery;

pub async fn handler(
  Query(PeripheryConnectionQuery { server }): Query<
    PeripheryConnectionQuery,
  >,
  headers: HeaderMap,
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  let server_id = crate::resource::get::<Server>(&server).await?.id;
  let query = format!("server={}", urlencoding::encode(&server));
  periphery_client::connection::server::handler(
    server_id, headers, query, ws,
  )
  .await
}
