# Robot Whisperer — from "Postman for robots" to a robotics tool

## Context

The GPUI/Rust rewrite shipped. What exists today, verified on screen: concurrent
connections (dummy, Foxglove WS, rosbridge, replay), a Postman-shaped request
model with collections and schema-driven payload forms, subscribe/publish/call/
send-goal, a command palette, dashboards with a real dock, a wgpu renderer
drawing point clouds and URDF robots, and record & replay. 535 tests,
`clippy -D warnings` clean.

The goal now is a tool people choose over RViz 2, Rerun and Foxglove Studio.
Reading the code for what is actually missing turns up one structural hole that
makes everything 3D subtly wrong, and a set of daily-driver gaps:

- **There is no TF anywhere in the workspace.** Grepping for `tf2_msgs`,
  `TFMessage` or a transform tree finds nothing outside message-parsing tests.
  Every 3D thing is drawn at the origin in whatever frame it arrived in. A scan
  is in `laser_link`, the map in `map`, the robot in `base_link` — so a cloud and
  a robot cannot share a scene correctly. This is RViz's actual core value and we
  do not have it.
- **A scene holds one of each thing.** `rw_render::Scene` has a singular
  `points: Points` and a `solids: Vec<Solid>`. One pane shows one topic. RViz's
  model — one world, many displays — is not expressible.
- **`VisualizationRole` is declared and never read.** `rw_canonical::viz`
  computes 15 roles from schema names, and nothing in `rw-ui` consults it;
  `views::visualize` sniffs ad-hoc (image → cloud → field table). The registry
  the old app had was never rebuilt.
- Dead code from the JS era: `rw-core/src/visualization/marker_array.rs` works on
  raw CDR payloads and is referenced by nothing in the new UI.
- No parameters (`RequestKind` is Topic/Service/Action only), no topic Hz or
  bandwidth, no log console, no time travel on live data.

## Scope of this plan

In scope, in order: **TF and one 3D world**, then **freeze & diff**, then
**daily-driver ergonomics**.

**Frozen — deliberately not now:**

- **Native ROS 1 (TCPROS) and native ROS 2 (DDS-RTPS) transports.** Both are
  real and both are wanted eventually; neither works in a browser, so `rw-web`
  would keep the bridges regardless. ROS 1 is the cheap one — the TCPROS
  connection header carries `message_definition`, so `rw-schema-ros1` and
  `rw-codec-rosmsg` get schemas for free. ROS 2 is the expensive one — DDS does
  not put message definitions on the wire, so types have to come from a local
  install's ament index. Neither changes what the user sees; they only change how
  bytes arrive. Revisit after the work below lands.
- **MCAP** read/write.

---

## Phase 1 — TF and one 3D world

The spine. Everything else leans on it, and it removes code as well as adding
it: the robot viewer and the point-cloud pane both become layers.

### 1.1 `crates/rw-tf` — the transform buffer (pure, tested)

New crate, no GPU and no UI, following the shape of `rw-assets` (parsing and
arithmetic only).

```rust
pub struct Transform { pub translation: [f32; 3], pub rotation: Quat }
pub struct Buffer { /* per-edge ring of stamped transforms, plus statics */ }

impl Buffer {
    pub fn insert(&mut self, parent: &str, child: &str, at_ns: u64, t: Transform);
    pub fn insert_static(&mut self, parent: &str, child: &str, t: Transform);
    pub fn lookup(&self, target: &str, source: &str, at_ns: u64) -> Result<Transform, TfError>;
    pub fn tree(&self) -> Vec<Node>;              // for the TF tree view
    pub fn age_ns(&self, frame: &str, now_ns: u64) -> Option<u64>;
}
```

Quaternions are new — `rw_assets::math` has matrices but no quats, and rotations
**must** slerp between samples, not lerp as matrices. `Transform::to_mat4()`
returns `[[f32; 4]; 4]`, which is the same concrete type as both
`rw_assets::math::Mat4` and `rw_render::Mat4`, so it interoperates with each
without either crate depending on the other.

Tests are the point of this crate:

- interpolation between the two samples bracketing a time,
- **extrapolation refused** with a typed error naming both frames and the gap —
  this is RViz's single most common error and a bare `None` is useless,
- a `map → odom → base_link → laser` chain composes,
- a static transform matches at any time,
- an unreachable frame says *which* frame broke the chain,
- a cycle terminates,
- the buffer is bounded and drops samples older than its window.

### 1.2 TF from the wire — `rw-ui/src/tf.rs`

`tf2_msgs/TFMessage` decoded from `CanonicalValue`, the same way
`rw-ui/src/cloud.rs` reads `PointCloud2`. An `Entity<TfStore>` holding one
`Buffer` per connection joins `workspace`/`sessions`/`runs`/`gpu`/`recorder` in
the `RobotWhisperer` global (`rw-ui/src/session.rs`).

A connection subscribes to `/tf` and `/tf_static` by itself when discovery
advertises them. That is a deliberate behaviour change and it is what RViz does.

### 1.3 `rw_render::Scene` becomes layered

```rust
pub struct Scene { pub camera: Camera, pub layers: Vec<Layer>, pub grid: Option<Grid>, … }
pub struct Layer { pub transform: Mat4, pub content: Content }
pub enum Content { Points(Points), Solids(Vec<Solid>), Lines(Vec<LineSet>), Axes(Vec<Axis>) }
```

Each layer carries the transform that places it in the fixed frame, applied at
draw time — so a moving robot is a matrix per frame, not a re-upload. The
existing geometry cache in `rw-render/src/lib.rs` (keyed by `Solid.key`) already
does exactly this for robot links.

**No new GPU work.** `rw-render` already has a point pipeline, a `LineList`
pipeline and a lit-triangle pipeline; axes are three coloured lines and markers
reuse `rw_assets::shapes::{cuboid, cylinder, sphere}`. Text markers are the one
thing that cannot be drawn and are out of scope for this phase.

### 1.4 `rw-ui/src/panels/world.rs` — the world pane

Replaces `panels/robot.rs` and the cloud path in `views::visualize`. Holds:

- a **fixed frame** selector — the most important control in RViz,
- a list of **layers**, each `(connection, topic, kind)` or a robot from the
  catalog, added by dragging a topic in from the sidebar or from `+` on the tab
  strip (`Panel::toolbar_buttons`, as the dashboard already does),
- the scene, everything resolved into the fixed frame via `Buffer::lookup`,
- a layer whose transform cannot be resolved is dimmed and says why, rather than
  being silently drawn at the origin.

Per the established discipline: **no settings tree, and no submenus.** A layer's
controls are the handful of things that matter, on the flat menu the dock
already draws. No row of chrome that only repeats what the tab title says.

Layer kinds for the first cut, chosen by `VisualizationRole`:
`PointCloud2` and `LaserScan` → points · `Marker`/`MarkerArray` → lines, points
and primitives · `Path` → a line strip · `Pose`/`PoseStamped`/`Odometry` → an
axis triad · `Tf` → every frame as a small triad · URDF robot → solids, each
link placed by its frame name.

### 1.5 The visualizer registry — `rw-ui/src/viz.rs`

Replaces the sniffing in `views::visualize` with a lookup on the
`VisualizationRole` that `rw_canonical::viz_role_for_schema` already computes,
falling back to the field table. A topic then honestly offers *several* views
(a cloud can be raw text, a field table, or 3D) instead of one guess.

Delete `crates/rw-core/src/visualization/marker_array.rs`; marker decoding moves
to `rw-ui` on `CanonicalValue`, beside `cloud.rs` and `image.rs`.

### 1.6 The dummy transport gains a world

`crates/rw-transport-dummy/src/lib.rs` grows `/tf`, `/tf_static`, `/scan`,
`/path` and `/pose`, so all of the above is drivable in the screenshot harness
with no robot present.

---

## Phase 2 — Freeze & diff

The differentiator, and cheap given the canonical value tree.

- `rw-ui/src/diff.rs`: walk two `CanonicalValue`s into
  `Vec<Change { path, before, after, delta }>`. Pure and tested — unchanged
  branches collapse, added and removed keys are marked, numeric fields carry a
  delta. Reuses `value::leaves`.
- **Freeze** pins the current message on a request or a pane; the live stream
  keeps running and a fourth view beside Raw/Visualize/Plot shows what has
  changed since. Freezing again re-pins.

---

## Phase 3 — daily-driver ergonomics

Small, high-frequency, interleave with the phases above.

- **Parameters** — a fourth `RequestKind::Param` with get/set. On ROS 2 these are
  ordinary `rcl_interfaces` services, so this works over rosbridge and Foxglove
  **today**, with no native transport.
- **Topic stats** — Hz, bandwidth and latency per subscription, computed in
  `rw-pipeline` from `Frame.timestamp_ns` against arrival time. `ros2 topic hz`
  is the most-run command in robotics and this is roughly 150 lines.
- **Log console** — `rcl_interfaces/Log` in the existing console panel, filtered
  by level and node.
- **Searchable topic picker** — replaces the flat menu on a pane's tab strip,
  which will not survive a robot with 300 topics.
- **Drag a topic from the sidebar into a pane** — the sidebar's GPUI drag and
  drop already works; the pane just needs to accept the drop.

---

## Critical files

| Area | Path |
| --- | --- |
| New: transform buffer | `crates/rw-tf/src/{lib,quat,buffer}.rs` |
| New: TF decoding and store | `crates/rw-ui/src/tf.rs` |
| New: world pane | `crates/rw-ui/src/panels/world.rs` (replaces `robot.rs`) |
| New: visualizer registry | `crates/rw-ui/src/viz.rs` |
| New: diff | `crates/rw-ui/src/diff.rs` |
| Layered scene | `crates/rw-render/src/scene.rs`, `lib.rs` |
| Global state | `crates/rw-ui/src/session.rs` |
| Dispatch to replace | `crates/rw-ui/src/views.rs` (`visualize`) |
| Delete | `crates/rw-core/src/visualization/marker_array.rs` |
| Synthetic world | `crates/rw-transport-dummy/src/lib.rs` |
| Stats | `crates/rw-pipeline/src/lib.rs` |
| Param request kind | `crates/rw-core/src/domain/request.rs` + storage migration |

## Reuse rather than rebuild

- `rw_assets::shapes::{cuboid, cylinder, sphere}` for marker primitives.
- `rw_assets::kinematics::solve` already places URDF links; the world pane feeds
  its output through TF instead of a bare correction matrix.
- `rw_render`'s geometry cache (`Solid.key`, `Renderer::forget`) already uploads
  once and re-places per frame.
- `rw-ui/src/cloud.rs` and `image.rs` are the pattern for decoding a
  `CanonicalValue` into something drawable — TF and markers follow it.
- `rw_canonical::viz_role_for_schema` already classifies every schema.
- `rw-record`'s `Cursor` and the replay transport are the model for any future
  timeline work.
- `crates/rw-ws` is the cross-platform WebSocket client.

## UI discipline (non-negotiable)

These were learned the hard way and apply to every pane added below:

- **No submenus.** Flat menus only.
- **No duplicated titles.** If the tab says it, the body does not repeat it.
- **No chrome rows that carry no information.** Zero-chrome panes: the topic is
  the tab title, counts go in `Panel::title_suffix`, controls go in the flat
  `Panel::dropdown_menu`.
- **No section for something that has no content** — e.g. no payload editor for a
  topic that takes no arguments.
- Every pane uses the same card material as the rest of the app
  (`tokens::card`), and its content fills the pane rather than floating in it.

## Verification

Every phase ends green on `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo test --workspace`, plus a screenshot
proving it on screen — the harness and its scenario DSL already exist
(`cargo xtask screenshot-native <scenario>`, `xtask/scenarios/*.txt`).

- **rw-tf**: unit tests as listed in 1.1 — interpolation, refused extrapolation,
  chain composition, statics, unreachable frames, cycles, bounded buffer.
- **World pane**: a new `xtask/scenarios/world.txt` against the dummy transport —
  connect, add a cloud layer and a robot layer, screenshot them correctly placed
  relative to each other, change the fixed frame, screenshot the scene move.
  This is the proof TF works: before it, the two sit on top of each other.
- **Registry**: a test asserting each `VisualizationRole` maps to a view, and
  that an unknown schema falls back to the field table.
- **Diff**: unit tests over `CanonicalValue` pairs, plus a `diff.txt` scenario —
  freeze on `/dummy/counter`, wait, screenshot the changed field and its delta.
- **Stats**: a test feeding timestamped frames and asserting the Hz estimate;
  on screen in the sidebar row.

## Risks

- **TF is where the bugs will be.** Frame conventions, time semantics and
  extrapolation are the classic sources of "it looks nearly right". The pure
  crate with adversarial tests is the mitigation, and the dummy world gives a
  known-correct answer to compare against.
- **Scene layering touches the renderer's public API**, so the point-cloud and
  robot panes change together with it. They are covered by scenarios, so the
  regression surface is visible rather than silent.
- Software rendering under lavapipe proves correctness, never speed. Any
  performance claim needs real hardware.
