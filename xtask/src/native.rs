//! Drives the native app under Xvfb and captures what it renders.
//!
//! Every incantation here was learned the hard way during Gate 0, and each one
//! is a silent failure if omitted:
//!
//! - **No window manager.** GPUI maps its window and then paints nothing until an
//!   input event arrives. The first screenshot is blank without a pointer nudge,
//!   which looks exactly like a rendering failure and is not one.
//! - **`xdotool` wants root coordinates.** Scenario coordinates are relative to
//!   the app window, so the window's origin is added before every move.
//! - **Capture by window id.** Capturing the root window catches the bare X
//!   background as often as the app.
//! - **Pin lavapipe.** The image ships several Vulkan ICDs and only the software
//!   one works on a host with no GPU.
//! - **`XDG_RUNTIME_DIR` must exist**, or Vulkan initialisation complains.
//! - **The window is never *active*.** With no window manager nothing sets input
//!   focus on it, so GPUI reports no focused element even though keystrokes are
//!   delivered and land in whichever input holds focus internally. Anything
//!   gated on focus — a blinking caret, `InputEvent::Focus` — will not happen
//!   here, and a feature that depends on it cannot be verified by this harness.
//!
//! The run is hermetic: `XDG_CONFIG_HOME` and `XDG_DATA_HOME` point into a fresh
//! directory, so a scenario always starts from an empty workspace and cannot be
//! influenced by a previous run.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

use crate::scenario::{Scenario, Step};

const LAVAPIPE_ICD: &str = "/usr/share/vulkan/icd.d/lvp_icd.json";
/// The `rw-desktop` crate's binary target.
const APP_BINARY: &str = "target/debug/robot-whisperer";
/// How long to wait for the app to map a window before giving up.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to keep nudging before giving up on a first frame.
const FIRST_PAINT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to leave between nudges while waiting for that frame.
const NUDGE_INTERVAL: Duration = Duration::from_millis(500);

pub struct Options {
    /// X display number to claim, e.g. `99`.
    pub display: u32,
    pub width: u32,
    pub height: u32,
    /// Where screenshots are written.
    pub out_dir: PathBuf,
    /// Theme to preseed, so a run does not have to click through a menu.
    pub theme: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            display: 99,
            width: 1600,
            height: 1000,
            out_dir: PathBuf::from("target/screenshots"),
            theme: None,
        }
    }
}

/// Runs one scenario and returns the screenshots it wrote.
pub fn run(scenario: &Scenario, options: &Options) -> Result<Vec<PathBuf>> {
    let display = claim_display(options)?;
    std::fs::create_dir_all(&options.out_dir)
        .with_context(|| format!("creating {}", options.out_dir.display()))?;

    let mut xvfb = spawn_xvfb(options, &display)?;
    let result = with_app(scenario, options, &display);
    let _ = xvfb.kill();
    let _ = xvfb.wait();
    result
}

fn with_app(scenario: &Scenario, options: &Options, display: &str) -> Result<Vec<PathBuf>> {
    let home = prepare_home(options)?;
    let log_path = home.join("rw-desktop.log");
    let log = std::fs::File::create(&log_path)
        .with_context(|| format!("creating {}", log_path.display()))?;
    let mut app = spawn_app(display, &home, log)?;

    let outcome = (|| {
        let window = wait_for_window(display, &mut app)?;
        // GPUI chooses its own window size rather than filling the screen, and
        // scenario coordinates are written against it, so it is reported.
        println!(
            "  window {} at {}x{}",
            window.id, window.width, window.height
        );
        await_first_paint(display, &window)?;
        play(
            scenario, options, display, &home, &log_path, &mut app, window,
        )
    })();

    let _ = app.kill();
    let _ = app.wait();

    outcome.with_context(|| match std::fs::read_to_string(&log_path) {
        Ok(log) if !log.trim().is_empty() => format!("rw-desktop said:\n{}", log.trim_end()),
        _ => format!("rw-desktop logged nothing to {}", log_path.display()),
    })
}

#[allow(clippy::too_many_arguments)]
fn play(
    scenario: &Scenario,
    options: &Options,
    display: &str,
    home: &Path,
    log_path: &Path,
    app: &mut Child,
    mut window: WindowGeometry,
) -> Result<Vec<PathBuf>> {
    let mut shots = Vec::new();

    for step in &scenario.steps {
        match step {
            Step::Move { x, y } => {
                point_at(display, &window, *x, *y)?;
            }
            Step::Click { x, y } => {
                point_at(display, &window, *x, *y)?;
                xdotool(display, &["click", "1"])?;
                // GPUI processes input on its own tick; give it one.
                std::thread::sleep(Duration::from_millis(250));
            }
            Step::RightClick { x, y } => {
                point_at(display, &window, *x, *y)?;
                xdotool(display, &["click", "3"])?;
                std::thread::sleep(Duration::from_millis(250));
            }
            Step::Drag { from, to } => {
                drag_to(display, &window, *from, *to)?;
                xdotool(display, &["mouseup", "1"])?;
                std::thread::sleep(Duration::from_millis(400));
            }
            Step::DragOver { from, to } => {
                drag_to(display, &window, *from, *to)?;
                // Left held, so the next step can photograph the target.
                std::thread::sleep(Duration::from_millis(250));
            }
            Step::Release => {
                xdotool(display, &["mouseup", "1"])?;
                std::thread::sleep(Duration::from_millis(400));
            }
            Step::Type { text } => {
                xdotool(display, &["type", "--delay", "40", text])?;
                std::thread::sleep(Duration::from_millis(250));
            }
            Step::Key { key } => {
                xdotool(display, &["key", key])?;
                std::thread::sleep(Duration::from_millis(250));
            }
            Step::Scroll { by } => {
                let button = if *by < 0 { "4" } else { "5" };
                for _ in 0..by.abs() {
                    xdotool(display, &["click", button])?;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Step::Wait { duration } => std::thread::sleep(*duration),
            Step::Restart => {
                let _ = app.kill();
                let _ = app.wait();
                // The window id is gone with the process, and the new one has
                // to be found before anything can be pointed at or captured.
                let log = std::fs::OpenOptions::new()
                    .append(true)
                    .open(log_path)
                    .with_context(|| format!("reopening {}", log_path.display()))?;
                *app = spawn_app(display, home, log)?;
                window = wait_for_window(display, app)?;
                await_first_paint(display, &window)?;
            }
            Step::Shot { name } => {
                let path = options.out_dir.join(format!("{name}.png"));
                capture(display, &window.id, &path)?;
                println!("  {}", path.display());
                shots.push(path);
            }
        }
    }

    Ok(shots)
}

// ── environment ────────────────────────────────────────────────────────────────

/// A fresh config and data home, so runs cannot contaminate each other.
///
/// Absolute, and `XDG_RUNTIME_DIR` is mode 0700: both are conditions the X and
/// Vulkan client libraries check, and a relative path is rejected with a message
/// that names neither.
fn prepare_home(options: &Options) -> Result<PathBuf> {
    let home =
        std::path::absolute(options.out_dir.join("home")).context("resolving the run directory")?;
    if home.exists() {
        std::fs::remove_dir_all(&home).with_context(|| format!("clearing {}", home.display()))?;
    }
    let config = home.join("config").join("robot-whisperer");
    std::fs::create_dir_all(&config).with_context(|| format!("creating {}", config.display()))?;
    std::fs::create_dir_all(home.join("data"))?;
    let runtime = home.join("runtime");
    std::fs::create_dir_all(&runtime)?;
    #[cfg(unix)]
    std::fs::set_permissions(
        &runtime,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .with_context(|| format!("restricting {}", runtime.display()))?;

    if let Some(theme) = &options.theme {
        // The same shape `Prefs` writes: `{"theme": "<name>"}`, where the theme
        // may be the `system` sentinel.
        let prefs = serde_json::json!({ "theme": theme });
        std::fs::write(config.join("prefs.json"), prefs.to_string())
            .context("seeding the theme preference")?;
    }

    Ok(home)
}

/// Whether anything is already serving this display.
fn display_is_busy(display: &str) -> bool {
    Command::new("xdpyinfo")
        .env("DISPLAY", display)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// How many display numbers to try before giving up.
const DISPLAYS_TO_TRY: u32 = 16;

/// Finds a display number nobody is serving, starting at the requested one.
///
/// This exists because of a failure that wasted a great deal of somebody's
/// time. The display number used to be hard-coded, and `spawn_xvfb` treated
/// "xdpyinfo can reach it" as proof that *its own* Xvfb had come up. Both of
/// those are false when something is already there:
///
/// - Two runs at once. The second Xvfb exits immediately with "server is
///   already active", `xdpyinfo` succeeds against the *first* one, and the
///   second run attaches to a display it does not own, finds the other run's
///   window, and hangs.
/// - A run that was killed rather than allowed to finish. Nothing runs the
///   cleanup, so its Xvfb and its app are still on the display, and the next
///   run inherits that mess — which is why cancelling a stuck run and starting
///   another one gets a second stuck run.
///
/// Claiming a free number makes both harmless: concurrent runs get a display
/// each, and a leftover is stepped over rather than joined.
fn claim_display(options: &Options) -> Result<String> {
    for offset in 0..DISPLAYS_TO_TRY {
        let display = format!(":{}", options.display + offset);
        if !display_is_busy(&display) {
            if offset > 0 {
                println!("  :{} is busy; using {display}", options.display);
            }
            return Ok(display);
        }
    }
    bail!(
        "displays :{}..:{} are all busy — something from an earlier run is \
         still holding them (`pkill Xvfb` clears it)",
        options.display,
        options.display + DISPLAYS_TO_TRY - 1
    );
}

fn spawn_xvfb(options: &Options, display: &str) -> Result<Child> {
    let screen = format!("{}x{}x24", options.width, options.height);
    let mut child = Command::new("Xvfb")
        .args([display, "-screen", "0", &screen, "-nolisten", "tcp"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting Xvfb — is it installed?")?;

    // Xvfb has no readiness signal; poll until xdpyinfo can reach the display.
    // The exit check is the other half: without it, an Xvfb that died on
    // startup still looks ready as long as *somebody* is serving the display.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().context("polling Xvfb")? {
            bail!("Xvfb exited on {display} before it was ready ({status})");
        }
        if display_is_busy(display) {
            return Ok(child);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("Xvfb did not come up on {display}");
}

/// Builds the native app, so a screenshot always shows the working tree.
pub fn build() -> Result<()> {
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "rw-desktop"])
        .current_dir(crate::workspace_root())
        .status()
        .context("running cargo build")?;
    if !status.success() {
        bail!("`cargo build -p rw-desktop` failed ({status})");
    }
    Ok(())
}

fn spawn_app(display: &str, home: &Path, log: std::fs::File) -> Result<Child> {
    // The crate is `rw-desktop`; the binary it installs is `robot-whisperer`.
    let binary = crate::workspace_root().join(APP_BINARY);
    if !binary.exists() {
        bail!("{} is missing", binary.display());
    }

    Command::new(binary)
        .env("DISPLAY", display)
        // Removed, not blanked: gpui_platform picks Wayland whenever the variable
        // is *present*, and an empty value leaves it waiting on a socket that
        // will never answer — a silent hang with nothing in any log.
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_RUNTIME_DIR", home.join("runtime"))
        .env("VK_DRIVER_FILES", LAVAPIPE_ICD)
        .env("RUST_BACKTRACE", "1")
        // Kept rather than inherited: when a run fails, the app's own log is the
        // first thing worth reading, and it must survive the process exiting.
        .stdout(log.try_clone().context("duplicating the log handle")?)
        .stderr(log)
        .spawn()
        .context("launching rw-desktop")
}

// ── the window ─────────────────────────────────────────────────────────────────

struct WindowGeometry {
    id: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn wait_for_window(display: &str, app: &mut Child) -> Result<WindowGeometry> {
    let deadline = Instant::now() + WINDOW_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = app.try_wait().context("checking on rw-desktop")? {
            bail!("rw-desktop exited before mapping a window ({status})");
        }
        if let Some(id) = find_window(display)? {
            return window_geometry(display, id);
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    bail!(
        "no application window appeared within {WINDOW_TIMEOUT:?}; X had:\n{}",
        children(display).unwrap_or_else(|error| format!("(xwininfo failed: {error:#})"))
    );
}

/// The app's window, picked out of the root's children.
///
/// A child is listed as `0x200001 (has no name): ()  1440x900+82+50  +82+50`.
/// GPUI also creates 1x1 helper windows, so the app is the one with real size —
/// and the window is never the root, which is why capture happens by id.
fn find_window(display: &str) -> Result<Option<String>> {
    let listing = children(display)?;
    Ok(listing
        .lines()
        .skip_while(|line| !line.contains("children:"))
        .find_map(|line| {
            let mut tokens = line.split_whitespace();
            let id = tokens.next().filter(|id| id.starts_with("0x"))?;
            let (width, _) = tokens.find_map(size_of)?;
            (width > 100).then(|| id.to_string())
        }))
}

fn children(display: &str) -> Result<String> {
    let listing = Command::new("xwininfo")
        .env("DISPLAY", display)
        .args(["-root", "-children"])
        .output()
        .context("running xwininfo")?;
    Ok(String::from_utf8_lossy(&listing.stdout).into_owned())
}

/// Reads a `WIDTHxHEIGHT+X+Y` token, ignoring anything that is not one.
fn size_of(token: &str) -> Option<(u32, u32)> {
    let (size, _) = token.split_once('+')?;
    let (width, height) = size.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn window_geometry(display: &str, id: String) -> Result<WindowGeometry> {
    let info = Command::new("xwininfo")
        .env("DISPLAY", display)
        .args(["-id", &id])
        .output()
        .context("reading window geometry")?;
    let text = String::from_utf8_lossy(&info.stdout);
    Ok(WindowGeometry {
        id,
        // Scenario coordinates are window-relative; xdotool wants root
        // coordinates, and the two differ by this origin.
        x: field(&text, "Absolute upper-left X:").unwrap_or(0),
        y: field(&text, "Absolute upper-left Y:").unwrap_or(0),
        width: field(&text, "Width:").unwrap_or_default().max(0) as u32,
        height: field(&text, "Height:").unwrap_or_default().max(0) as u32,
    })
}

fn field(text: &str, label: &str) -> Option<i32> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix(label))
        .and_then(|value| value.trim().parse().ok())
}

fn point_at(display: &str, window: &WindowGeometry, x: i32, y: i32) -> Result<()> {
    let (x, y) = (window.x + x, window.y + y);
    xdotool(display, &["mousemove", &x.to_string(), &y.to_string()])?;
    // Hover states are part of what these screenshots verify, so let them settle.
    std::thread::sleep(Duration::from_millis(150));
    Ok(())
}

fn capture(display: &str, id: &str, path: &Path) -> Result<()> {
    let status = Command::new("import")
        .env("DISPLAY", display)
        .args(["-window", id, "-quality", "95"])
        .arg(path)
        .status()
        .context("running ImageMagick `import`")?;
    if !status.success() {
        bail!("capturing {} failed ({status})", path.display());
    }
    Ok(())
}

fn xdotool(display: &str, args: &[&str]) -> Result<()> {
    let status = Command::new("xdotool")
        .env("DISPLAY", display)
        .args(args)
        .status()
        .context("running xdotool")?;
    if !status.success() {
        bail!("xdotool {args:?} failed ({status})");
    }
    Ok(())
}

/// Presses at `from` and drags to `to`, leaving the button held.
///
/// Stepped rather than jumped: a toolkit starts a drag only once the pointer has
/// travelled while held, and it needs those moves as separate events to notice.
fn drag_to(display: &str, window: &WindowGeometry, from: (i32, i32), to: (i32, i32)) -> Result<()> {
    const STEPS: i32 = 8;

    point_at(display, window, from.0, from.1)?;
    xdotool(display, &["mousedown", "1"])?;
    std::thread::sleep(Duration::from_millis(120));

    for step in 1..=STEPS {
        let x = from.0 + (to.0 - from.0) * step / STEPS;
        let y = from.1 + (to.1 - from.1) * step / STEPS;
        point_at(display, window, x, y)?;
        std::thread::sleep(Duration::from_millis(40));
    }
    Ok(())
}

/// Nudges the pointer until the window actually draws something.
///
/// GPUI paints on demand, and a mapped window is not a painted one: without this
/// a scenario whose first step is a screenshot captures a black rectangle, which
/// looks exactly like a broken layout. Waiting a fixed time was the old approach
/// and it failed whenever the machine was busy — so this checks the frame instead
/// of guessing at a duration, and only gives up after
/// [`FIRST_PAINT_TIMEOUT`].
fn await_first_paint(display: &str, window: &WindowGeometry) -> Result<()> {
    let probe = std::env::temp_dir().join(format!("rw-first-paint-{}.png", std::process::id()));
    let deadline = std::time::Instant::now() + FIRST_PAINT_TIMEOUT;
    let mut at = 10;

    while std::time::Instant::now() < deadline {
        // From a different point each time: a motion event to where the pointer
        // already is tells the toolkit nothing. A bare modifier press goes with
        // it — GPUI does not always treat pointer motion over an unfocused
        // window as a reason to draw, and a key event with no binding costs
        // nothing but does.
        at = if at == 10 { 40 } else { 10 };
        xdotool(display, &["mousemove", &at.to_string(), &at.to_string()])?;
        xdotool(display, &["key", "shift"])?;
        std::thread::sleep(NUDGE_INTERVAL);

        capture(display, &window.id, &probe)?;
        if colours(&probe)? > 1 {
            let _ = std::fs::remove_file(&probe);
            return Ok(());
        }
    }

    let _ = std::fs::remove_file(&probe);
    bail!("the window never painted anything (still one flat colour after {FIRST_PAINT_TIMEOUT:?})")
}

/// How many distinct colours a capture holds. One means nothing was drawn.
fn colours(path: &Path) -> Result<u32> {
    let out = Command::new("identify")
        .args(["-format", "%k"])
        .arg(path)
        .output()
        .context("running identify")?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .context("reading the colour count from identify")
}
