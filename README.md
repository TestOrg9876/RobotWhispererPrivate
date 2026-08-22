# Robot Whisperer

Robot Whisperer is a Postman-style client for ROS. It gives you one interface to connect to a robot, browse its topics, services, and actions, send and inspect messages, and build live dashboards from the data, without writing throwaway scripts or running `ros2 topic echo` across a dozen terminals.

Built with [GPUI](https://www.gpui.rs/) on a Rust core that compiles to both a native binary and WebAssembly, so the desktop app and the browser build are the same code with the same renderer.

<p align="center">
    <img src="./images/themed_robot_whisperer.png" alt="Robot Whisperer" width="640"/>
</p>

## Features

- **Requests.** Subscribe to topics, call services, send or cancel action goals, and read or write parameters. Payloads are entered through a schema-driven form built from the definition *this* connection sent, and results stream into a live view.
- **Views of a response.** Pretty (the form read back), Raw, Visualize, Plot, Diff (freeze and watch what moves), and History — past runs of a request, with their arguments restored into the form by clicking one.
- **Message definitions.** Click the schema chip for the whole bundle, nested types and all, as the robot described it — `ros2 interface show`, but for the system in front of you rather than for whatever is installed locally.
- **3D world.** TF-resolved layers in one scene: point clouds, laser scans, markers, paths, poses, the transform tree itself, URDF robots from the catalog, and the robot the system publishes on `/robot_description`. A layer that will not resolve is dimmed and says why rather than being drawn in the wrong place.
- **Transform tree.** Every frame, its parent, and how long ago each edge last published — `view_frames` and `tf_monitor` without leaving the app.
- **Dashboards.** Arrange panes in resizable splits, drag a topic in from the sidebar, and save the layout.
- **Record and replay.** Capture live topics to a file, reopen it as a connection, and play it back with a transport bar: pause, scrub, speed, loop.
- **Console.** The app's own notices and the robot's `/rosout`, in one ordering, filtered by level.
- **Workspace import/export.** Connections, requests, and collections are stored in SQLite (native) or IndexedDB (web) and can be exported and imported as human-readable JSON.

## Connectivity

Connections are defined by a transport and a URL.

- **Foxglove WebSocket.** Connects to `foxglove_bridge` for both ROS 1 and ROS 2.
- **rosbridge.** The rosbridge v2 protocol over WebSocket, for both ROS 1 and ROS 2.
- **Replay.** A recording, opened as a connection like any other.
- **Dummy.** An offline transport that emits synthetic topics, services, and actions, so you can try the app or run tests with no robot available.

Native ROS 1 (TCPROS) and ROS 2 (DDS) transports are out of scope for now.

## Architecture

A single runtime-agnostic pipeline sits behind every transport. It keeps exactly one upstream subscription per `(connection, topic)` and ref-counts a zero-copy fan-out to all consumers, and exposes a uniform command surface (open/close, subscribe, call service, send goal).

The UI is GPUI, and 3D is drawn by `rw-render` on wgpu. There is no webview and no IPC: the native app is one process, and the web build is the same crates compiled to `wasm32-unknown-unknown` with the same renderer on WebGPU.

Schemas are resolved per connection rather than by name, because a name is not an identity — a ROS 1 `std_msgs/Header` carries `seq` and a ROS 2 one does not, and both are in the registry the moment both robots are connected.

## Getting started

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (the toolchain is pinned in `rust-toolchain.toml`)
- For web builds: the `wasm32-unknown-unknown` target and a `wasm-bindgen` CLI matching the version in `Cargo.lock` (`cargo xtask wasm-bindgen-version` prints it)

No Node, no bundler, and no system webview.

### Run

Desktop:

```shell
cargo run -p rw-desktop --release
```

With no robot at hand, add a **Dummy** connection from the sidebar to stream synthetic data.

Web:

```shell
cargo xtask web --dev --serve
```

## Building

```shell
cargo build -p rw-desktop --release   # native binary
cargo xtask web                       # static site under target/web
```

The web output is self-contained and deployable to any static host.

## Development

```shell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The UI is also covered by a screenshot suite that drives the real app under Xvfb:

```shell
cargo xtask list-scenarios
cargo xtask screenshot-native --out target/shots
```

Open the PNGs. A scenario that runs to completion proves nothing — see `docs/knowledge-base.md`, which is the durable document for this codebase.

## License

Released under the [MIT License](./LICENSE).
