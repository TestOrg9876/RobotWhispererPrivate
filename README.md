# Robot Whisperer

Robot Whisperer is a Postman-style client for ROS. It gives you one interface to connect to a robot, browse its topics, services, and actions, send and inspect messages, and build live dashboards from the data, without writing throwaway scripts or running `ros2 topic echo` across a dozen terminals.

Built with SvelteKit (Svelte 5) and Electron on top of a Rust core that compiles to both a native binary and WebAssembly, it runs as a desktop app and in the browser from a single codebase.

> [!NOTE]
> Try it now in your browser at [ros.heroicwaffle.dev](https://ros.heroicwaffle.dev).

<p align="center">
    <img src="./images/themed_robot_whisperer.png" alt="Robot Whisperer" width="640"/>
</p>

## Features

- **Requests.** Subscribe to topics, call services, and send or cancel action goals. Message payloads are entered through a schema-driven form, and results stream into a live view.
- **Visualizers.** Any compatible request can be rendered, not just shown as JSON: images (`sensor_msgs/Image`, including compressed), point clouds (`visualization_msgs/MarkerArray`), and streaming plots of any numeric field.
- **Dashboards.** Arrange panes in resizable splits, group them into tabs, drag to re-dock, maximize, and go fullscreen. Layouts are persisted locally.
- **Built-in panes.** Raw/JSON, streaming Plot, Image, and Point Cloud, all sharing the same visualizer layer as the request view.
- **Custom panes.** A small plugin API: a pane is a Svelte component plus a descriptor, with topic subscription and service/action calls provided through a narrow context.
- **Workspace import/export.** Connections, requests, and collections are stored in SQLite (native) or IndexedDB (web) and can be exported and imported as human-readable JSON.
- **Theming.** Seven built-in themes, applied instantly and persisted.

## Connectivity

Connections are defined by a transport and a URL.

- **Foxglove WebSocket.** Connects to `foxglove_bridge` for both ROS 1 and ROS 2.
- **rosbridge.** The rosbridge v2 protocol over WebSocket, for both ROS 1 and ROS 2.
- **Dummy.** An offline transport that emits synthetic topics, services, and actions, so you can try the app or run tests with no robot available.

Native ROS 2 (via `rclrs`) is planned.

## Architecture

A single runtime-agnostic pipeline sits behind every transport. It keeps exactly one upstream subscription per `(connection, topic)` and ref-counts a zero-copy fan-out to all consumers, decodes messages off the main thread, and exposes a uniform command surface (open/close, subscribe, call service, send goal). This core lives in a multi-crate Rust workspace under `core/` and is shared by two front ends: the desktop build runs it as a standalone daemon (`rw-daemon`) and talks to it over a loopback WebSocket, and the web build calls the same code compiled to WebAssembly. The frontend is therefore identical across desktop and web.

### The desktop shell

Electron owns the window and the daemon's lifetime, and nothing else — it is deliberately **not** in the data path:

```
Renderer (Svelte, Chromium)
  ├─ decoder Web Worker ──ws──► rw-daemon   binary frames, the hot path
  └─ JSON-RPC           ──ws──► rw-daemon   commands
Electron main: spawns the daemon, owns the window.
```

ROS frames go straight from Rust into a Web Worker over loopback and never cross Electron's IPC boundary. Because the core is a plain binary rather than a Node addon, it has no Node ABI coupling, needs no rebuild per Electron version, and can be driven with no UI at all — `node scripts/daemon-smoke.mjs` does exactly that, and runs in CI.

Both sockets require a per-launch token and pass an `Origin` check. This matters more than it looks: browsers do not apply CORS to `ws://127.0.0.1:<port>`, so an unauthenticated loopback socket is reachable by any website the user happens to have open.

Electron was chosen over Tauri because Tauri renders through the _system_ webview (WebKitGTK on Linux), which behaves differently on every distro and is too old to run the app at all on Ubuntu 20.04. Electron ships its own Chromium, so one build behaves identically everywhere. See [`bench/comparison.md`](./bench/comparison.md) for the measured before/after.

## Getting started

### Prerequisites

- [Bun](https://bun.com/docs/installation)
- [Rust](https://www.rust-lang.org/tools/install)
- For web/WASM builds: [`wasm-pack`](https://crates.io/crates/wasm-pack) (`cargo install wasm-pack`). It adds the `wasm32-unknown-unknown` target and runs `wasm-opt` for you
- For desktop builds: nothing beyond Bun and Rust. Electron ships its own runtime, so there are no system GUI development packages to install

### Install

```shell
git clone https://github.com/Mika412/RobotWhisperer.git
cd RobotWhisperer
bun install
```

### Run

Web (development):

```shell
bun run web
```

This builds the WASM module and serves the app at `http://localhost:5173` with hot reload. With no robot at hand, add a **Dummy** connection from the sidebar to stream synthetic data. `bun run web` uses a development WASM build. Use `bun run web:release` to run the optimized build.

Desktop (development):

```shell
bun run start
```

This builds the renderer, the Rust daemon and the Electron shell, then launches the app.

## Building

Web (static site):

```shell
bun run build
```

Outputs an optimized, self-contained site to `build/`, deployable to any static host. Serve it locally with `bun run preview`.

Desktop:

```shell
bun run package            # every Linux format
bun run package:deb        # or one at a time
bun run package:appimage
bun run package:snap       # needs snapd
bun run package:flatpak    # needs flatpak-builder and flathub
```

Artifacts are written to `dist/`.

`deb` and `AppImage` build anywhere. `snap` shells out to the real `snapcraft`, and `flatpak` pulls its runtime from flathub, so both need those tools present — the [release workflow](./.github/workflows/release.yml) builds all four on a GitHub runner and uploads them, which is the easiest way to get a full set. That workflow also installs the resulting `.deb` on Ubuntu 20.04, 22.04 and 24.04 and launches it, so backwards compatibility is tested rather than assumed.

## Development

```shell
bun run check     # svelte-check (types)
bun run lint      # eslint
bun run test      # vitest unit tests
bun run format    # prettier --write
```

For the Rust workspace, in `core/`:

```shell
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

To exercise the core end to end with no frontend:

```shell
cargo build --release -p rw-daemon
node scripts/daemon-smoke.mjs core/target/release/rw-daemon
```

## License

Released under the [MIT License](./LICENSE).
