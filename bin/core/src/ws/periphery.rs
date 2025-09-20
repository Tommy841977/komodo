use axum::{
  extract::{Query, WebSocketUpgrade},
  response::Response,
};
use komodo_client::entities::server::Server;
use transport::PeripheryConnectionQuery;

pub async fn handler(
  Query(PeripheryConnectionQuery { server }): Query<
    PeripheryConnectionQuery,
  >,
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  let server = crate::resource::get::<Server>(&server).await?;
  periphery_client::connection::server::handler(server.id, ws).await
}
