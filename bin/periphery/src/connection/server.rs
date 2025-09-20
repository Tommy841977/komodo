use std::{
  net::{IpAddr, SocketAddr},
  str::FromStr,
};

use anyhow::{Context, anyhow};
use axum::{
  Router,
  body::Body,
  extract::{ConnectInfo, WebSocketUpgrade},
  http::{Request, StatusCode},
  middleware::{self, Next},
  response::Response,
  routing::get,
};
use axum_server::tls_rustls::RustlsConfig;
use bytes::Bytes;
use serror::{AddStatusCode, AddStatusCodeError};
use transport::{
  auth::handle_server_side_login,
  websocket::{
    WebsocketMessage, WebsocketReceiver, WebsocketSender,
    axum::AxumWebsocket,
  },
};

use crate::{
  config::periphery_config,
  connection::{handle_incoming_bytes, ws_receiver},
};

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

async fn handler(ws: WebSocketUpgrade) -> serror::Result<Response> {
  // Limits to only one active websocket connection.
  let mut write_receiver = ws_receiver()
    .try_lock()
    .status_code(StatusCode::FORBIDDEN)?;

  Ok(ws.on_upgrade(|socket| async move {
    let mut socket = AxumWebsocket(socket);

    if let Err(e) =
      handle_server_side_login(&mut socket, |b| true).await
    {
      warn!("Client failed to login | {e:#}");
      return;
    };

    let (mut ws_write, mut ws_read) = socket.split();

    let forward_writes = async {
      loop {
        let msg = match write_receiver.recv().await {
          // Sender Dropped (shouldn't happen, it is static).
          None => break,
          // This has to copy the bytes to follow ownership rules.
          Some(msg) => Bytes::copy_from_slice(msg),
        };
        match ws_write.send(msg).await {
          // Clears the stored message from receiver buffer.
          // TODO: Move after response ack.
          Ok(_) => write_receiver.clear_buffer(),
          Err(e) => {
            warn!("Failed to send response | {e:?}");
            let _ = ws_write.close(None).await;
            break;
          }
        }
      }
    };

    let handle_reads = async {
      loop {
        match ws_read.recv().await {
          Ok(WebsocketMessage::Binary(bytes)) => {
            handle_incoming_bytes(bytes).await
          }
          Ok(WebsocketMessage::Close(frame)) => {
            warn!("Connection closed with frame: {frame:?}");
            break;
          }
          Ok(WebsocketMessage::Closed) => {
            warn!("Connection already closed");
            break;
          }
          Err(e) => {
            error!("Failed to read websocket message | {e:?}");
            break;
          }
        };
      }
    };

    tokio::select! {
      _ = forward_writes => {},
      _ = handle_reads => {},
    };
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
