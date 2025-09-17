use anyhow::{Context, anyhow};
use axum::{
  Router,
  body::Body,
  extract::ConnectInfo,
  http::{Request, StatusCode},
  middleware::{self, Next},
  response::Response,
  routing::get,
};
use serror::{AddStatusCode, AddStatusCodeError};
use std::net::{IpAddr, SocketAddr};

use crate::config::periphery_config;

pub fn router() -> Router {
  Router::new()
    .route("/", get(crate::connection::inbound_connection))
    .layer(middleware::from_fn(guard_request_by_ip))
}

async fn guard_request_by_ip(
  req: Request<Body>,
  next: Next,
) -> serror::Result<Response> {
  if periphery_config().allowed_ips.is_empty() {
    return Ok(next.run(req).await);
  }
  let ConnectInfo(socket_addr) = req
    .extensions()
    .get::<ConnectInfo<SocketAddr>>()
    .context("could not get ConnectionInfo of request")
    .status_code(StatusCode::UNAUTHORIZED)?;
  let ip = socket_addr.ip();

  let ip_match = periphery_config().allowed_ips.iter().any(|net| {
    net.contains(ip)
      || match ip {
        IpAddr::V4(ipv4) => {
          net.contains(IpAddr::V6(ipv4.to_ipv6_mapped()))
        }
        IpAddr::V6(_) => net.contains(ip.to_canonical()),
      }
  });

  if ip_match {
    Ok(next.run(req).await)
  } else {
    Err(
      anyhow!("requesting ip {ip} not allowed")
        .status_code(StatusCode::UNAUTHORIZED),
    )
  }
}
