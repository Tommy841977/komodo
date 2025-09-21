use axum::extract::ws::CloseFrame;
use futures_util::{
  SinkExt, Stream, StreamExt, TryStreamExt,
  stream::{SplitSink, SplitStream},
};

use super::{
  Websocket, WebsocketMessage, WebsocketReceiver, WebsocketSender,
};

pub struct AxumWebsocket(pub axum::extract::ws::WebSocket);

impl Websocket for AxumWebsocket {
  type CloseFrame = CloseFrame;
  type Error = axum::Error;

  fn split(self) -> (impl WebsocketSender, impl WebsocketReceiver) {
    let (tx, rx) = self.0.split();
    (AxumWebsocketSender(tx), AxumWebsocketReceiver(rx))
  }

  async fn recv(
    &mut self,
  ) -> Result<WebsocketMessage<Self::CloseFrame>, Self::Error> {
    try_next(&mut self.0).await
  }

  async fn send(
    &mut self,
    bytes: bytes::Bytes,
  ) -> Result<(), Self::Error> {
    self.0.send(axum::extract::ws::Message::Binary(bytes)).await
  }

  async fn close(
    &mut self,
    frame: Option<Self::CloseFrame>,
  ) -> Result<(), Self::Error> {
    self.0.send(axum::extract::ws::Message::Close(frame)).await
  }
}

pub type InnerWebsocketReceiver =
  SplitStream<axum::extract::ws::WebSocket>;

pub struct AxumWebsocketReceiver(pub InnerWebsocketReceiver);

impl WebsocketReceiver for AxumWebsocketReceiver {
  type CloseFrame = CloseFrame;
  type Error = axum::Error;

  async fn recv(
    &mut self,
  ) -> Result<WebsocketMessage<Self::CloseFrame>, Self::Error> {
    try_next(&mut self.0).await
  }
}

pub type InnerWebsocketSender =
  SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>;

pub struct AxumWebsocketSender(pub InnerWebsocketSender);

impl WebsocketSender for AxumWebsocketSender {
  type CloseFrame = CloseFrame;
  type Error = axum::Error;

  async fn send(
    &mut self,
    bytes: bytes::Bytes,
  ) -> Result<(), Self::Error> {
    self.0.send(axum::extract::ws::Message::Binary(bytes)).await
  }

  async fn close(
    &mut self,
    frame: Option<Self::CloseFrame>,
  ) -> Result<(), Self::Error> {
    self.0.send(axum::extract::ws::Message::Close(frame)).await
  }
}

async fn try_next<S>(
  stream: &mut S,
) -> Result<WebsocketMessage<CloseFrame>, axum::Error>
where
  S: Stream<Item = Result<axum::extract::ws::Message, axum::Error>>
    + Unpin,
{
  loop {
    match stream.try_next().await? {
      Some(axum::extract::ws::Message::Binary(bytes)) => {
        return Ok(WebsocketMessage::Binary(bytes));
      }
      Some(axum::extract::ws::Message::Text(text)) => {
        return Ok(WebsocketMessage::Binary(text.into()));
      }
      Some(axum::extract::ws::Message::Close(frame)) => {
        return Ok(WebsocketMessage::Close(frame));
      }
      None => return Ok(WebsocketMessage::Closed),
      // Ignored messages
      Some(axum::extract::ws::Message::Ping(_))
      | Some(axum::extract::ws::Message::Pong(_)) => continue,
    }
  }
}
