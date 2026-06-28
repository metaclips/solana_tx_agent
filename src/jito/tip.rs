use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, interval};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

/// Live Jito tip data emitted by the tip stream.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct TipData {
    pub time: String,
    pub landed_tips_25th_percentile: f64,
    pub landed_tips_50th_percentile: f64,
    pub landed_tips_75th_percentile: f64,
    pub landed_tips_95th_percentile: f64,
    pub landed_tips_99th_percentile: f64,
    pub ema_landed_tips_50th_percentile: f64,
}

/// Create a new asynchronous receiver for Jito tip updates.
pub async fn new_jito_tip_stream() -> tokio::sync::mpsc::UnboundedReceiver<TipData> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut retries = 0;
        let mut ping_interval = interval(Duration::from_secs(20));

        loop {
            match connect().await {
                Ok((mut write, mut read)) => {
                    retries = 0;
                    let no_tip_timer = tokio::time::sleep(Duration::from_millis(300));
                    tokio::pin!(no_tip_timer);

                    loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(e)) => match e {
                                        Message::Binary(data) => {
                                            if let Ok(tip) = decode_ws_tip(&data) {
                                                let _ = sender.send(tip);
                                                no_tip_timer.as_mut().reset(Instant::now() + Duration::from_millis(300));
                                            }
                                        }
                                        Message::Text(text) => {
                                            if let Ok(tip) = decode_ws_tip(text.as_bytes()) {
                                                let _ = sender.send(tip);
                                                no_tip_timer.as_mut().reset(Instant::now() + Duration::from_millis(300));
                                            }
                                        }
                                        Message::Ping(payload) => {
                                            let _ = write.send(Message::Pong(payload)).await;
                                        }
                                        tokio_tungstenite::tungstenite::Message::Close(_) => break,
                                        _ => {}
                                    },
                                    Some(Err(e)) => {
                                        warn!("Jito tip websocket error {e}");
                                        break;
                                    }
                                    None => break,
                                }
                            }
                            _ = &mut no_tip_timer => {
                                if let Ok(tip) = get_tip_data_via_reqwest().await {
                                    let _ = sender.send(tip);
                                }
                                no_tip_timer.as_mut().reset(Instant::now() + Duration::from_millis(300));
                            }
                            _ = ping_interval.tick() => {
                                let _ = write.send(tokio_tungstenite::tungstenite::Message::Ping(Vec::new().into())).await;
                            }
                        }
                    }
                }
                Err(_) => {
                    retries += 1;
                    if retries >= 5 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });
    receiver
}

#[inline]
fn decode_ws_tip(tip_data: &[u8]) -> anyhow::Result<TipData> {
    let mut decoded: Vec<TipData> = serde_json::from_slice(tip_data)?;
    if decoded.is_empty() {
        anyhow::bail!("Failed to decode tip data")
    }
    decoded
        .pop()
        .ok_or_else(|| anyhow::anyhow!("Failed to decode tip data"))
}

async fn connect() -> anyhow::Result<(
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
    futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
)> {
    info!("Starting Jito tip stream");
    let (tip_stream, _) = connect_async("wss://bundles.jito.wtf/api/v1/bundles/tip_stream").await?;
    Ok(tip_stream.split())
}

/// Fallback HTTP lookup for Jito tip data.
pub async fn get_tip_data_via_reqwest() -> anyhow::Result<TipData> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://bundles.jito.wtf/api/v1/bundles/tip_floor")
        .send()
        .await?;
    let bytes = resp.bytes().await?;
    decode_ws_tip(&bytes)
}
