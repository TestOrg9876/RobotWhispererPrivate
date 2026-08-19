//! Robot Whisperer core daemon.
//!
//! The whole Rust pipeline — transports, codecs, schema registry, SQLite
//! workspace — behind a loopback WebSocket. The desktop shell (Electron) spawns
//! this and talks to it; the browser build compiles the same crates to WASM and
//! calls them in-process. Nothing in here knows what the frontend is, which is
//! the point: the core is genuinely frontend-agnostic and can be driven by a
//! script with no UI at all.
//!
//! Startup handshake: the port and auth token are printed to stdout as one
//! line of JSON, then stdout is never used again. Logs go to stderr so they
//! cannot corrupt that line.

mod ingest;
mod rpc;
mod server;
mod state;
mod wire;

use std::path::PathBuf;

use state::{resolve_data_dir, AppState};

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("os entropy");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_data_dir() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--data-dir=") {
            return Some(PathBuf::from(value));
        }
        if arg == "--data-dir" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

/// Exit when the parent closes our stdin. Without this, killing or crashing the
/// shell would leave an orphaned daemon holding the workspace database and a
/// listening socket.
async fn exit_with_parent() {
    use tokio::io::AsyncReadExt;
    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 64];
    loop {
        match stdin.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => continue,
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let data_dir = resolve_data_dir(parse_data_dir());
    let state = match AppState::bootstrap(data_dir).await {
        Ok(state) => state,
        Err(err) => {
            tracing::error!(%err, "failed to start");
            eprintln!("rw-daemon: {err}");
            std::process::exit(1);
        }
    };

    let token = generate_token();
    let (port, server) = match server::serve(state, token.clone()).await {
        Ok(pair) => pair,
        Err(err) => {
            tracing::error!(?err, "failed to bind loopback listener");
            eprintln!("rw-daemon: bind failed: {err}");
            std::process::exit(1);
        }
    };

    // The shell blocks on this line, so it must be printed only once the
    // listener is actually accepting.
    println!("{}", serde_json::json!({ "port": port, "token": token }));
    use std::io::Write;
    let _ = std::io::stdout().flush();

    tokio::select! {
        _ = exit_with_parent() => tracing::info!("parent closed stdin, shutting down"),
        _ = server => tracing::warn!("server task ended"),
    }
}
