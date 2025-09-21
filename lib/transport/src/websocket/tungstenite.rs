use futures_util::{
  SinkExt, Stream, StreamExt, TryStreamExt,
  stream::{SplitSink, SplitStream},
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
  MaybeTlsStream, WebSocketStream,
  tungstenite::{self, protocol::CloseFrame},
};

use super::{
  Websocket, WebsocketMessage, WebsocketReceiver, WebsocketSender,
};

pub type InnerWebsocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct TungsteniteWebsocket(pub InnerWebsocket);

impl Websocket for TungsteniteWebsocket {
  type CloseFrame = CloseFrame;
  type Error = tungstenite::Error;

  fn split(self) -> (impl WebsocketSender, impl WebsocketReceiver) {
    let (tx, rx) = self.0.split();
    (
      TungsteniteWebsocketSender(tx),
      TungsteniteWebsocketReceiver(rx),
    )
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
    self.0.send(tungstenite::Message::Binary(bytes)).await
  }

  async fn close(
    &mut self,
    frame: Option<Self::CloseFrame>,
  ) -> Result<(), Self::Error> {
    self.0.close(frame).await
  }
}

pub type InnerWebsocketReceiver =
  SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

pub struct TungsteniteWebsocketReceiver(pub InnerWebsocketReceiver);

impl WebsocketReceiver for TungsteniteWebsocketReceiver {
  type CloseFrame = CloseFrame;
  type Error = tungstenite::Error;

  async fn recv(
    &mut self,
  ) -> Result<WebsocketMessage<Self::CloseFrame>, Self::Error> {
    try_next(&mut self.0).await
  }
}

pub type InnerWebsocketSender = SplitSink<
  WebSocketStream<MaybeTlsStream<TcpStream>>,
  tungstenite::Message,
>;

pub struct TungsteniteWebsocketSender(pub InnerWebsocketSender);

impl WebsocketSender for TungsteniteWebsocketSender {
  type CloseFrame = CloseFrame;
  type Error = tungstenite::Error;

  async fn send(
    &mut self,
    bytes: bytes::Bytes,
  ) -> Result<(), Self::Error> {
    self.0.send(tungstenite::Message::Binary(bytes)).await
  }

  async fn close(
    &mut self,
    frame: Option<Self::CloseFrame>,
  ) -> Result<(), Self::Error> {
    self.0.send(tungstenite::Message::Close(frame)).await
  }
}

async fn try_next<S>(
  stream: &mut S,
) -> Result<WebsocketMessage<CloseFrame>, tungstenite::Error>
where
  S: Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
    + Unpin,
{
  loop {
    match stream.try_next().await? {
      Some(tungstenite::Message::Binary(bytes)) => {
        return Ok(WebsocketMessage::Binary(bytes));
      }
      Some(tungstenite::Message::Text(text)) => {
        return Ok(WebsocketMessage::Binary(text.into()));
      }
      Some(tungstenite::Message::Close(frame)) => {
        return Ok(WebsocketMessage::Close(frame));
      }
      None => return Ok(WebsocketMessage::Closed),
      // Ignored messages
      Some(tungstenite::Message::Ping(_))
      | Some(tungstenite::Message::Pong(_))
      | Some(tungstenite::Message::Frame(_)) => continue,
    }
  }
}
