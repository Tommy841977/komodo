use std::{
  net::{IpAddr, SocketAddr},
  str::FromStr,
};

use anyhow::{Context, anyhow};
use axum::{
  Router,
  body::Body,
  extract::{ConnectInfo, WebSocketUpgrade},
  http::{HeaderMap, Request, StatusCode},
  middleware::{self, Next},
  response::Response,
  routing::get,
};
use axum_server::tls_rustls::RustlsConfig;
use serror::{AddStatusCode, AddStatusCodeError};
use transport::{
  auth::{ConnectionIdentifiers, ServerLoginFlow, compute_accept},
  websocket::axum::AxumWebsocket,
};

use crate::{config::periphery_config, connection::ws_receiver};

pub async fn run() -> anyhow::Result<()> {
  let config = periphery_config();

  let addr = format!("{}:{}", config.bind_ip, config.port);

  let socket_addr = SocketAddr::from_str(&addr)
    .context("failed to parse listen address")?;

  let app = Router::new()
    .route("/", get(crate::connection::server::handler))
    .layer(middleware::from_fn(guard_request_by_ip))
    .into_make_service_with_connect_info::<SocketAddr>();

  if config.ssl_enabled {
    info!("🔒 Periphery SSL Enabled");
    rustls::crypto::ring::default_provider()
      .install_default()
      .expect("failed to install default rustls CryptoProvider");
    crate::helpers::ensure_ssl_certs().await;
    info!("Komodo Periphery starting on https://{}", socket_addr);
    let ssl_config = RustlsConfig::from_pem_file(
      config.ssl_cert_file(),
      config.ssl_key_file(),
    )
    .await
    .context("Invalid ssl cert / key")?;
    axum_server::bind_rustls(socket_addr, ssl_config)
      .serve(app)
      .await?
  } else {
    info!("🔓 Periphery SSL Disabled");
    info!("Komodo Periphery starting on http://{}", socket_addr);
    axum_server::bind(socket_addr).serve(app).await?
  }

  Ok(())
}

async fn handler(
  mut headers: HeaderMap,
  ws: WebSocketUpgrade,
) -> serror::Result<Response> {
  // Limits to only one active websocket connection.
  let mut write_receiver = ws_receiver()
    .try_lock()
    .status_code(StatusCode::FORBIDDEN)?;

  let host = headers
    .remove("x-forwarded-host")
    .or(headers.remove("host"))
    .context("Failed to get connection host")
    .status_code(StatusCode::UNAUTHORIZED)?;
  let ws_key = headers
    .remove("sec-websocket-key")
    .context("Headers do not contain Sec-Websocket-Key")
    .status_code(StatusCode::UNAUTHORIZED)?;
  let ws_accept = compute_accept(ws_key.as_bytes());

  Ok(ws.on_upgrade(|socket| async move {
    let socket = AxumWebsocket(socket);

    let connection_identifiers = ConnectionIdentifiers {
      host: host.as_bytes(),
      query: &[],
      accept: ws_accept.as_bytes(),
    };

    // TODO: source the pk
    if let Err(e) = super::handle_websocket::<ServerLoginFlow>(
      socket,
      connection_identifiers,
      "TEST",
      &mut write_receiver,
    )
    .await
    {
      warn!("Core failed to login to connection | {e:#}");
      return;
    }
  }))
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
