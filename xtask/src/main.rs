//! Project tasks: the screenshot harnesses that verify each milestone.
//!
//! These live here rather than in a session's shell history because every
//! milestone is verified the same way, and the environment needs half a dozen
//! non-obvious settings to render at all.

use xtask::load_bridge;
mod native;
mod scenario;
mod web;

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};

use scenario::Scenario;

/// The workspace root, resolved from this crate's manifest rather than the
/// working directory, so `cargo xtask` behaves the same from anywhere.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  screenshot-native [scenario…]   drive the native app under Xvfb and capture it
  load-bridge [options]           synthetic Foxglove/rosbridge server for benchmarks
  web [--dev] [--serve]           build the wasm bundle, optionally serving it
  screenshot-web [scenario…]      replay the same scenarios in Chromium
  list-scenarios                  show the committed scenarios
  wasm-bindgen-version            print the wasm-bindgen version the lockfile pins

options for screenshot-native:
  --theme <name|system>   preseed the theme preference (default: the app's own)
  --out <dir>             where to write PNGs (default: target/screenshots)
  --display <n>           X display to claim (default: 99)
  --size <WxH>            window size (default: 1600x1000)
";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("screenshot-native") => screenshot_native(args.collect()),
        Some("load-bridge") => load_bridge::main(args.collect()),
        Some("web") => build_web(args.collect()),
        Some("screenshot-web") => screenshot_web(args.collect()),
        Some("wasm-bindgen-version") => web::print_bindgen_version(),
        Some("list-scenarios") => list_scenarios(),
        Some("--help") | Some("-h") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => {
            print!("{USAGE}");
            bail!("unknown task `{other}`");
        }
    }
}

fn build_web(args: Vec<String>) -> Result<()> {
    // Release by default: an unoptimised wasm module is over 130 MiB and the
    // browser spends longer compiling it than any scenario takes to run.
    let mut release = true;
    let mut serve = false;
    let mut port = 3000;
    let mut rest = args.into_iter();

    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--dev" => release = false,
            "--serve" => serve = true,
            "--port" => {
                port = rest
                    .next()
                    .context("--port needs a value")?
                    .parse()
                    .context("--port needs a number")?
            }
            other => bail!("unknown option `{other}`"),
        }
    }

    let dist = web::build(release)?;
    if serve {
        web::serve(&dist, port)?;
    }
    Ok(())
}

fn screenshot_web(args: Vec<String>) -> Result<()> {
    let mut out_dir = workspace_root().join("target/screenshots/web");
    let mut port = 3000;
    let mut release = true;
    let mut names = Vec::new();
    let mut rest = args.into_iter();

    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--out" => {
                out_dir = PathBuf::from(rest.next().context("--out needs a value")?);
            }
            "--port" => {
                port = rest
                    .next()
                    .context("--port needs a value")?
                    .parse()
                    .context("--port needs a number")?
            }
            "--dev" => release = false,
            flag if flag.starts_with('-') => bail!("unknown option `{flag}`"),
            name => names.push(name.to_string()),
        }
    }

    if names.is_empty() {
        names = available()?;
    }

    for name in &names {
        let scenario = load(name)?;
        println!(
            "{name}: {} steps, {} screenshots",
            scenario.steps.len(),
            scenario.shots().len()
        );
        web::screenshot(&scenario, &out_dir, port, release)?;
    }
    Ok(())
}

fn scenario_dir() -> PathBuf {
    workspace_root().join("xtask/scenarios")
}

fn list_scenarios() -> Result<()> {
    for name in available()? {
        let scenario = load(&name)?;
        println!(
            "{name}  ({} steps) -> {}",
            scenario.steps.len(),
            scenario.shots().join(", ")
        );
    }
    Ok(())
}

fn available() -> Result<Vec<String>> {
    let dir = scenario_dir();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "txt").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();
    Ok(names)
}

fn load(name: &str) -> Result<Scenario> {
    let path = scenario_dir().join(format!("{name}.txt"));
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Scenario::parse(name, &source).with_context(|| format!("in {}", path.display()))
}

fn screenshot_native(args: Vec<String>) -> Result<()> {
    let mut options = native::Options::default();
    let mut names = Vec::new();
    let mut rest = args.into_iter();

    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| -> Result<String> {
            rest.next().with_context(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--theme" => options.theme = Some(value("--theme")?),
            "--out" => options.out_dir = PathBuf::from(value("--out")?),
            "--display" => {
                options.display = value("--display")?
                    .parse()
                    .context("--display needs a number")?
            }
            "--size" => {
                let size = value("--size")?;
                let (width, height) = size
                    .split_once('x')
                    .context("--size looks like 1600x1000")?;
                options.width = width.parse().context("width is not a number")?;
                options.height = height.parse().context("height is not a number")?;
            }
            flag if flag.starts_with('-') => bail!("unknown option `{flag}`"),
            name => names.push(name.to_string()),
        }
    }

    if names.is_empty() {
        names = available()?;
    }

    native::build()?;

    for name in &names {
        let scenario = load(name)?;
        println!(
            "{name}: {} steps, {} screenshots",
            scenario.steps.len(),
            scenario.shots().len()
        );
        let shots = native::run(&scenario, &options)?;
        if shots.is_empty() {
            bail!("{name} took no screenshots — every scenario should capture something");
        }
    }
    Ok(())
}
