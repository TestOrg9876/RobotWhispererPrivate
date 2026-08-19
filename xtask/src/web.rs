//! Builds the wasm bundle and serves it.
//!
//! Two things here are not incidental. The wasm target needs **nightly** —
//! `gpui_web` depends on zed's `wasm_thread` fork, which opens with
//! `feature(stdarch_wasm_atomic_wait)` — so the toolchain is selected
//! explicitly rather than inherited from `rust-toolchain.toml`. And the static
//! server is written here rather than borrowed, because `.wasm` must be served
//! as `application/wasm` for `WebAssembly.instantiateStreaming` to accept it,
//! and the usual one-line servers do not all know that type.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

use crate::scenario::Scenario;

/// Where `wasm-bindgen` output and the host page are assembled.
pub const DIST: &str = "target/web";
/// The JS module name the host page imports.
const MODULE: &str = "robot_whisperer";
const HOST_PAGE: &str = "crates/rw-web/www/index.html";

/// Builds `rw-web` for wasm32 and assembles a servable directory.
pub fn build(release: bool) -> Result<PathBuf> {
    let mut cargo = Command::new("rustup");
    cargo.args(["run", "nightly", "cargo", "build", "-p", "rw-web"]);
    cargo.args(["--target", "wasm32-unknown-unknown"]);
    if release {
        cargo.arg("--release");
    }
    cargo.current_dir(crate::workspace_root());
    let status = cargo.status().context("running the wasm cargo build")?;
    if !status.success() {
        bail!("the wasm build failed ({status})");
    }

    let profile = if release { "release" } else { "debug" };
    let root = crate::workspace_root();
    let wasm = root
        .join("target/wasm32-unknown-unknown")
        .join(profile)
        .join("rw_web.wasm");
    if !wasm.exists() {
        bail!("{} was not produced", wasm.display());
    }

    let dist = root.join(DIST);
    if dist.exists() {
        std::fs::remove_dir_all(&dist).with_context(|| format!("clearing {}", dist.display()))?;
    }
    std::fs::create_dir_all(&dist)?;

    check_bindgen_version()?;
    let status = Command::new("wasm-bindgen")
        .args(["--target", "web", "--out-name", MODULE, "--out-dir"])
        .arg(&dist)
        .arg(&wasm)
        .status()
        .context("running wasm-bindgen — is wasm-bindgen-cli installed?")?;
    if !status.success() {
        bail!("wasm-bindgen failed ({status})");
    }

    std::fs::copy(root.join(HOST_PAGE), dist.join("index.html"))
        .with_context(|| format!("copying {HOST_PAGE}"))?;

    let bytes = std::fs::metadata(dist.join(format!("{MODULE}_bg.wasm")))?.len();
    println!(
        "{}: {:.1} MiB wasm",
        dist.display(),
        bytes as f64 / (1 << 20) as f64
    );
    Ok(dist)
}

/// Fails early when the `wasm-bindgen` CLI does not match the `wasm-bindgen`
/// crate the lockfile pins.
///
/// The two must be the *same* version — the bindgen format is unstable — and
/// the mismatch only shows up after a full wasm build has already run.
fn check_bindgen_version() -> Result<()> {
    let installed = Command::new("wasm-bindgen")
        .arg("--version")
        .output()
        .context("running `wasm-bindgen --version` — is wasm-bindgen-cli installed?")?;
    let installed = String::from_utf8_lossy(&installed.stdout);
    let installed = installed.split_whitespace().nth(1).unwrap_or("").trim();

    let Some(locked) = locked_version("wasm-bindgen")? else {
        return Ok(());
    };
    if installed != locked {
        bail!(
            "wasm-bindgen CLI is {installed} but the lockfile pins {locked}; \
             the bindgen format is unstable, so they must match exactly — run \
             `cargo install wasm-bindgen-cli --version {locked} --locked`"
        );
    }
    Ok(())
}

/// The `wasm-bindgen` version the lockfile pins, for CI to install the matching
/// CLI rather than hard-coding a number that drifts.
pub fn print_bindgen_version() -> Result<()> {
    match locked_version("wasm-bindgen")? {
        Some(version) => {
            println!("{version}");
            Ok(())
        }
        None => bail!("wasm-bindgen is not in the dependency graph"),
    }
}

/// The version `Cargo.lock` pins for `name`, read without a TOML parser: the
/// lockfile's `[[package]]` blocks are plain `key = "value"` lines.
fn locked_version(name: &str) -> Result<Option<String>> {
    let path = crate::workspace_root().join("Cargo.lock");
    let lock =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut in_package = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_package = false;
        } else if let Some(value) = line.strip_prefix("name = ") {
            in_package = value.trim_matches('"') == name;
        } else if in_package && let Some(value) = line.strip_prefix("version = ") {
            return Ok(Some(value.trim_matches('"').to_string()));
        }
    }
    Ok(None)
}

/// Serves `root` until the process is killed.
pub fn serve(root: &Path, port: u16) -> Result<()> {
    let listener = listen(port)?;
    println!("serving {} on http://127.0.0.1:{port}/", root.display());
    for stream in listener.incoming() {
        let stream = stream.context("accepting a connection")?;
        // Sequential on purpose: this serves one browser during a screenshot
        // run, and a thread pool would be machinery with no user.
        if let Err(error) = respond(stream, root) {
            eprintln!("  request failed: {error:#}");
        }
    }
    Ok(())
}

pub fn listen(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port)).with_context(|| format!("binding port {port}"))
}

/// Serves one request from `root`, then closes the connection.
pub fn respond(mut stream: TcpStream, root: &Path) -> Result<()> {
    let mut request = String::new();
    BufReader::new(stream.try_clone()?)
        .read_line(&mut request)
        .context("reading the request line")?;

    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split(['?', '#']).next().unwrap_or("/");
    let relative = match path.trim_start_matches('/') {
        "" => "index.html",
        other => other,
    };

    // Nothing outside `root` is servable, whatever the request asks for.
    let Some(file) = resolve(root, relative) else {
        return reply(&mut stream, "404 Not Found", "text/plain", b"not found");
    };

    let mut body = Vec::new();
    std::fs::File::open(&file)
        .with_context(|| format!("opening {}", file.display()))?
        .read_to_end(&mut body)?;
    reply(&mut stream, "200 OK", content_type(&file), &body)
}

fn resolve(root: &Path, relative: &str) -> Option<PathBuf> {
    if relative.contains("..") {
        return None;
    }
    let candidate = root.join(relative);
    candidate.is_file().then_some(candidate)
}

/// `application/wasm` is the one that matters: `instantiateStreaming` rejects
/// anything else, and the failure reads as a generic wasm error.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("wasm") => "application/wasm",
        Some("js" | "mjs") => "text/javascript",
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn reply(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

// ── screenshots ────────────────────────────────────────────────────────────────

const DRIVER: &str = "xtask/scripts/drive-web.mjs";

/// Builds the bundle, serves it, and replays `scenario` against it in Chromium.
///
/// The server runs on this thread's own listener in a background thread rather
/// than as a child process, so it cannot outlive a failed run and leave a port
/// held — which is exactly the sort of thing that makes the *next* run fail for
/// an unrelated reason.
pub fn screenshot(scenario: &Scenario, out_dir: &Path, port: u16, release: bool) -> Result<()> {
    // A release wasm inlines the frames above a panic away, which leaves a stack
    // ending at `Instant::now` with no clue who called it. `--dev` trades a very
    // large module for a stack that names the caller.
    let dist = build(release)?;
    let listener = listen(port)?;

    let serving = dist.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Err(error) = respond(stream, &serving) {
                eprintln!("  request failed: {error:#}");
            }
        }
    });

    let steps = serde_json::to_string(scenario).context("serialising the scenario")?;
    let mut driver = Command::new("node")
        .arg(crate::workspace_root().join(DRIVER))
        .arg(format!("http://127.0.0.1:{port}/"))
        .arg(out_dir)
        .stdin(Stdio::piped())
        .spawn()
        .context("running node — is it on PATH?")?;
    driver
        .stdin
        .take()
        .context("the driver has no stdin")?
        .write_all(steps.as_bytes())
        .context("handing the scenario to the driver")?;

    let status = driver.wait().context("waiting for the web driver")?;
    if !status.success() {
        bail!("the web run failed ({status}) — see the reported panics above");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_is_served_as_application_wasm() {
        // instantiateStreaming refuses any other type, and the resulting error
        // says nothing about MIME types.
        assert_eq!(content_type(Path::new("a/b.wasm")), "application/wasm");
        assert_eq!(content_type(Path::new("a/b.js")), "text/javascript");
        assert_eq!(
            content_type(Path::new("a/b.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("a/b")), "application/octet-stream");
    }

    #[test]
    fn traversal_is_refused() {
        let root = Path::new("target/web");
        assert!(resolve(root, "../../etc/passwd").is_none());
        assert!(resolve(root, "a/../../b").is_none());
    }

    #[test]
    fn the_lockfile_pins_a_wasm_bindgen_version() {
        // Guards the reader in `locked_version`: a lockfile format change would
        // otherwise silently skip the whole check.
        let version = locked_version("wasm-bindgen")
            .expect("Cargo.lock is readable")
            .expect("wasm-bindgen is in the dependency graph");
        assert!(
            version.starts_with("0.2."),
            "unexpected wasm-bindgen version {version:?}"
        );
    }

    #[test]
    fn an_absent_package_has_no_locked_version() {
        assert_eq!(locked_version("definitely-not-a-crate").unwrap(), None);
    }

    #[test]
    fn a_missing_file_resolves_to_nothing() {
        assert!(resolve(Path::new("target/web"), "definitely-not-here.js").is_none());
    }
}
