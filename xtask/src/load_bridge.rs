//! A synthetic ROS bridge, for measuring what a client does under load.
//!
//! Speaks both wire protocols the app supports — the Foxglove WebSocket
//! protocol and rosbridge v2 — in both ROS 1 and ROS 2 dialects, and publishes
//! a configurable set of topics at a configurable rate.
//!
//! Synthetic rather than a real `foxglove_bridge` behind a `roscore` on
//! purpose. The question is what the *client* costs, and a real bridge adds its
//! own scheduling jitter, its own serialisation and a second process competing
//! for the same cores. Here the load is exact, reproducible, and identical for
//! both clients under test.
//!
//! What is faithful: the frame layout, the subprotocol names, the discovery
//! handshake, the schema text (a ROS 1 `Header` carries `seq`, a ROS 2 one does
//! not), and the encodings — `ros1` and `cdr` respectively. What is not: there
//! is no ROS graph behind it, so this measures transport, decode and render,
//! not `rclcpp`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

/// Which wire protocol to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Foxglove,
    Rosbridge,
}

/// Which ROS the bridge claims to be in front of.
///
/// Not cosmetic: it picks the subprotocol, the message encoding (`ros1` vs
/// `cdr`), and whether `std_msgs/Header` carries `seq`. A client that resolves
/// schemas by name alone gets this wrong the moment both are connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Ros1,
    Ros2,
}

/// One topic the bridge publishes.
#[derive(Debug, Clone)]
pub struct Stream {
    pub topic: String,
    pub schema_name: String,
    pub schema: String,
    pub hz: f64,
    /// Encoded payload, built once and re-sent. Building it per frame would
    /// measure this program rather than the client.
    pub payload: Arc<Vec<u8>>,
    /// JSON form, for rosbridge.
    pub json: Arc<serde_json::Value>,
}

pub struct Config {
    pub protocol: Protocol,
    pub dialect: Dialect,
    pub port: u16,
    pub streams: Vec<Stream>,
}

/// Bytes actually written to clients, so the report can say whether the load
/// was delivered or the server itself fell behind.
pub static SENT_BYTES: AtomicU64 = AtomicU64::new(0);
pub static SENT_FRAMES: AtomicU64 = AtomicU64::new(0);

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

pub fn header_schema(dialect: Dialect) -> &'static str {
    match dialect {
        // The difference that makes name-based schema resolution wrong.
        Dialect::Ros1 => "uint32 seq\ntime stamp\nstring frame_id\n",
        Dialect::Ros2 => "builtin_interfaces/Time stamp\nstring frame_id\n",
    }
}

pub async fn serve(config: Config) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", config.port))
        .await
        .with_context(|| format!("binding port {}", config.port))?;
    eprintln!(
        "load-bridge: {:?}/{:?} on ws://127.0.0.1:{} with {} streams",
        config.protocol,
        config.dialect,
        config.port,
        config.streams.len()
    );
    let config = Arc::new(config);
    loop {
        let (stream, _) = listener.accept().await?;
        let config = Arc::clone(&config);
        tokio::spawn(async move {
            if let Err(error) = match config.protocol {
                Protocol::Foxglove => foxglove(stream, config).await,
                Protocol::Rosbridge => rosbridge(stream, config).await,
            } {
                eprintln!("load-bridge: connection ended: {error:#}");
            }
        });
    }
}

async fn foxglove(stream: tokio::net::TcpStream, config: Arc<Config>) -> Result<()> {
    let wanted = match config.dialect {
        Dialect::Ros1 => "foxglove.websocket.v1",
        Dialect::Ros2 => "foxglove.sdk.v1",
    };
    let websocket = tokio_tungstenite::accept_hdr_async(stream, |_req: &Request, mut response: Response| {
        response.headers_mut().insert(
            "sec-websocket-protocol",
            wanted.parse().expect("static subprotocol"),
        );
        Ok(response)
    })
    .await
    .context("foxglove handshake")?;
    let (mut sink, mut source) = websocket.split();

    let encoding = match config.dialect {
        Dialect::Ros1 => "ros1",
        Dialect::Ros2 => "cdr",
    };
    let schema_encoding = match config.dialect {
        Dialect::Ros1 => "ros1msg",
        Dialect::Ros2 => "ros2msg",
    };

    sink.send(Message::Text(
        serde_json::json!({
            "op": "serverInfo",
            "name": "rw-load-bridge",
            "capabilities": [],
            "supportedEncodings": [encoding],
            "sessionId": "load",
        })
        .to_string(),
    ))
    .await?;

    let channels: Vec<serde_json::Value> = config
        .streams
        .iter()
        .enumerate()
        .map(|(index, stream)| {
            serde_json::json!({
                "id": index as u32 + 1,
                "topic": stream.topic,
                "encoding": encoding,
                "schemaName": stream.schema_name,
                "schema": stream.schema,
                "schemaEncoding": schema_encoding,
            })
        })
        .collect();
    sink.send(Message::Text(
        serde_json::json!({ "op": "advertise", "channels": channels }).to_string(),
    ))
    .await?;

    // channel id -> subscription id, filled by the client's `subscribe`.
    let subscriptions: Arc<tokio::sync::Mutex<HashMap<u32, u32>>> = Default::default();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(1024);

    {
        let subscriptions = Arc::clone(&subscriptions);
        tokio::spawn(async move {
            while let Some(Ok(message)) = source.next().await {
                let Message::Text(text) = message else { continue };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                match value.get("op").and_then(|op| op.as_str()) {
                    Some("subscribe") => {
                        let mut map = subscriptions.lock().await;
                        for entry in value["subscriptions"].as_array().into_iter().flatten() {
                            let (Some(id), Some(channel)) = (
                                entry["id"].as_u64(),
                                entry["channelId"].as_u64(),
                            ) else {
                                continue;
                            };
                            map.insert(channel as u32, id as u32);
                        }
                    }
                    Some("unsubscribe") => {
                        let mut map = subscriptions.lock().await;
                        for id in value["subscriptionIds"].as_array().into_iter().flatten() {
                            let Some(id) = id.as_u64() else { continue };
                            map.retain(|_, sub| *sub != id as u32);
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    for (index, stream) in config.streams.iter().enumerate() {
        let channel = index as u32 + 1;
        let period = Duration::from_secs_f64(1.0 / stream.hz.max(0.001));
        let payload = Arc::clone(&stream.payload);
        let subscriptions = Arc::clone(&subscriptions);
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(subscription) = subscriptions.lock().await.get(&channel).copied() else {
                    continue;
                };
                let mut frame = Vec::with_capacity(13 + payload.len());
                frame.push(0x01);
                frame.extend_from_slice(&subscription.to_le_bytes());
                frame.extend_from_slice(&now_ns().to_le_bytes());
                frame.extend_from_slice(&payload);
                SENT_BYTES.fetch_add(frame.len() as u64, Ordering::Relaxed);
                SENT_FRAMES.fetch_add(1, Ordering::Relaxed);
                if tx.send(Message::Binary(frame)).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    while let Some(message) = rx.recv().await {
        sink.send(message).await?;
    }
    Ok(())
}

async fn rosbridge(stream: tokio::net::TcpStream, config: Arc<Config>) -> Result<()> {
    let websocket = tokio_tungstenite::accept_async(stream)
        .await
        .context("rosbridge handshake")?;
    let (mut sink, mut source) = websocket.split();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(1024);
    let subscribed: Arc<tokio::sync::Mutex<HashMap<String, bool>>> = Default::default();

    {
        let config = Arc::clone(&config);
        let subscribed = Arc::clone(&subscribed);
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = source.next().await {
                let Message::Text(text) = message else { continue };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                match value.get("op").and_then(|op| op.as_str()) {
                    Some("subscribe") => {
                        if let Some(topic) = value["topic"].as_str() {
                            subscribed.lock().await.insert(topic.to_string(), true);
                        }
                    }
                    Some("unsubscribe") => {
                        if let Some(topic) = value["topic"].as_str() {
                            subscribed.lock().await.remove(topic);
                        }
                    }
                    Some("call_service") => {
                        let id = value["id"].clone();
                        let service = value["service"].as_str().unwrap_or_default();
                        let args = value["args"].clone();
                        let values = rosapi(&config, service, &args);
                        let reply = serde_json::json!({
                            "op": "service_response",
                            "id": id,
                            "service": service,
                            "values": values,
                            "result": true,
                        });
                        let _ = tx.send(Message::Text(reply.to_string())).await;
                    }
                    _ => {}
                }
            }
        });
    }

    for stream in &config.streams {
        let topic = stream.topic.clone();
        let json = Arc::clone(&stream.json);
        let period = Duration::from_secs_f64(1.0 / stream.hz.max(0.001));
        let subscribed = Arc::clone(&subscribed);
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Serialised once: rosbridge is JSON, and re-encoding a 1080p frame
            // per tick would measure this program instead of the client.
            let text = serde_json::json!({ "op": "publish", "topic": topic, "msg": *json })
                .to_string();
            loop {
                ticker.tick().await;
                if !subscribed.lock().await.contains_key(&topic) {
                    continue;
                }
                SENT_BYTES.fetch_add(text.len() as u64, Ordering::Relaxed);
                SENT_FRAMES.fetch_add(1, Ordering::Relaxed);
                if tx.send(Message::Text(text.clone())).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    while let Some(message) = rx.recv().await {
        sink.send(message).await?;
    }
    Ok(())
}

/// The `rosapi` surface the rosbridge transport needs for discovery.
fn rosapi(config: &Config, service: &str, args: &serde_json::Value) -> serde_json::Value {
    match service {
        "/rosapi/topics" => serde_json::json!({
            "topics": config.streams.iter().map(|s| s.topic.clone()).collect::<Vec<_>>(),
            "types": config.streams.iter().map(|s| s.schema_name.clone()).collect::<Vec<_>>(),
        }),
        "/rosapi/services" => serde_json::json!({ "services": [] }),
        "/rosapi/topic_type" => {
            let topic = args["topic"].as_str().unwrap_or_default();
            let found = config.streams.iter().find(|s| s.topic == topic);
            serde_json::json!({ "type": found.map(|s| s.schema_name.clone()).unwrap_or_default() })
        }
        "/rosapi/message_details" => {
            let wanted = args["type"].as_str().unwrap_or_default();
            let found = config.streams.iter().find(|s| s.schema_name == wanted);
            match found {
                Some(stream) => serde_json::json!({
                    "typedefs": [{
                        "type": stream.schema_name,
                        "fieldnames": [], "fieldtypes": [], "fieldarraylen": [],
                        "examples": [], "constnames": [], "constvalues": [],
                    }],
                }),
                None => serde_json::json!({ "typedefs": [] }),
            }
        }
        _ => serde_json::json!({}),
    }
}

/// `cargo xtask load-bridge --protocol foxglove --dialect ros2 --port 9001 …`
pub fn main(args: Vec<String>) -> Result<()> {
    let mut protocol = Protocol::Foxglove;
    let mut dialect = Dialect::Ros2;
    let mut port = 9001u16;
    let mut preset = String::from("chatter");
    let mut count = 1usize;
    let mut hz = 10.0f64;

    let mut rest = args.into_iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().context("missing value");
        match arg.as_str() {
            "--protocol" => {
                protocol = match value()?.as_str() {
                    "foxglove" => Protocol::Foxglove,
                    "rosbridge" => Protocol::Rosbridge,
                    other => bail!("unknown protocol {other}"),
                }
            }
            "--dialect" => {
                dialect = match value()?.as_str() {
                    "ros1" => Dialect::Ros1,
                    "ros2" => Dialect::Ros2,
                    other => bail!("unknown dialect {other}"),
                }
            }
            "--port" => port = value()?.parse()?,
            "--preset" => preset = value()?,
            "--count" => count = value()?.parse()?,
            "--hz" => hz = value()?.parse()?,
            other => bail!("unknown flag {other}"),
        }
    }

    let streams = crate::load_shapes::build(&preset, count, hz, dialect)?;
    let total: f64 = streams
        .iter()
        .map(|s| s.payload.len() as f64 * s.hz)
        .sum::<f64>()
        / 1_048_576.0;
    eprintln!("load-bridge: offered load {total:.1} MiB/s");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(Config {
        protocol,
        dialect,
        port,
        streams,
    }))
}
