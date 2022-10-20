use async_trait::async_trait;
use futures_channel::mpsc;
use futures_util::StreamExt;
use native_tls::TlsConnector;
use prost::Message;
use std::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio_tungstenite::{
    tungstenite::{self, client::IntoClientRequest},
    Connector::NativeTls,
};

use websocket_message::{
    webSocketMessage::Type, WebSocketMessage, WebSocketRequestMessage, WebSocketResponseMessage,
};

pub mod websocket_message;

const KEEPALIVE: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(40);

#[async_trait(?Send)]
pub trait WebSocketConnection {
    fn get_url(&self) -> &url::Url;
    fn get_tx(&self) -> &Option<mpsc::UnboundedSender<tungstenite::Message>>;
    fn set_tx(&mut self, tx: Option<mpsc::UnboundedSender<tungstenite::Message>>);
    fn get_last_keepalive(&self) -> Arc<Mutex<Instant>>;
    async fn on_message(&self, message: WebSocketMessage);

    async fn connect(&mut self, tls_connector: TlsConnector) {
        let mut request = self.get_url().into_client_request().unwrap();

        request
            .headers_mut()
            .insert("X-Signal-Agent", http::HeaderValue::from_static("\"OWA\""));

        let (ws_stream, _) = tokio_tungstenite::connect_async_tls_with_config(
            request,
            None,
            Some(NativeTls(tls_connector)),
        )
        .await
        .expect("Failed to connect");

        println!("WebSocket handshake has been successfully completed");

        // Websocket I/O
        let (ws_write, ws_read) = ws_stream.split();

        // channel to websocket ws_write
        let (tx, rx) = mpsc::unbounded();
        self.set_tx(Some(tx));
        let msg_handle = rx.map(Ok).forward(ws_write);

        let ws_handle = ws_read.for_each(|message| async {
            println!("> New message");
            if let Ok(message) = message {
                let data = message.into_data();
                let ws_message = match WebSocketMessage::decode(&data[..]) {
                    Err(_) => {
                        println!("Can't decode msg");
                        return ();
                    }
                    Ok(msg) => msg,
                };
                self.on_message(ws_message).await;
            }
        });

        // keepalive : channel, handler + thread
        let (timer_tx, timer_rx) = mpsc::unbounded();
        let (abort_tx, abort_rx) = mpsc::unbounded();

        let keepalive_handle = timer_rx.for_each(|_| async { self.send_keepalive() });
        let abort_handle = abort_rx.for_each(|_| async { panic!("Aborting.") });

        let last_keepalive = self.get_last_keepalive();
        thread::spawn(move || loop {
            if last_keepalive.lock().unwrap().elapsed() > KEEPALIVE_TIMEOUT {
                println!("Did not receive the last keepalive.");
                abort_tx.unbounded_send(true).unwrap();
            }
            thread::sleep(KEEPALIVE);
            println!("> Sending Keepalive");
            timer_tx.unbounded_send(true).unwrap();
        });

        // handle websocket
        let _ = futures::join!(msg_handle, ws_handle, keepalive_handle, abort_handle);
    }

    fn send(&self, message: WebSocketMessage) {
        if let Some(tx) = self.get_tx().as_ref() {
            let mut buf = Vec::new();
            buf.reserve(message.encoded_len());
            message.encode(&mut buf).unwrap();
            tx.unbounded_send(tungstenite::Message::Binary(buf))
                .unwrap();
        }
    }

    fn send_keepalive(&self) {
        println!("send_keepalive");
        let message = WebSocketMessage {
            r#type: Some(Type::REQUEST as i32),
            response: None,
            request: Some(WebSocketRequestMessage {
                verb: Some(String::from("GET")),
                path: Some(String::from("/v1/keepalive")),
                body: None,
                headers: Vec::new(),
                id: Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                ),
            }),
        };
        self.send(message);
    }

    fn send_response(&self, response: WebSocketResponseMessage) {
        let message = WebSocketMessage {
            r#type: Some(Type::RESPONSE as i32),
            response: Some(response),
            request: None,
        };
        self.send(message);
    }
}
