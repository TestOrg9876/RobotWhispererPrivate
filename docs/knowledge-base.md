# Robot Whisperer — knowledge base

The one durable document. Read this first; it replaces `roadmap.md` and
`handoff.md`, both of which were snapshots that went stale. Keep it updated as
part of the work, not afterwards.

**Branch: `claude/gpui-project-rewrite-ahwvje`.** `main` is the old
SvelteKit/Tauri app and holds none of this code — the Rust workspace exists only
on this branch. No pull request has been opened, deliberately.

**Push early.** A container restart on 2026-08-21 destroyed a commit that had
been made but not pushed, plus a working tree, and the recovery cost an hour of
retyping. Commit each self-contained piece and push it before starting the next.
Unpushed work does not exist.

---

## 1. What this is

"Postman for robots": a desktop and web app for talking to ROS systems. You save
requests against topics, services, actions and node parameters the way Postman
saves HTTP calls, arrange live views into dashboards, and visualise what comes
back in 2D and 3D.

The goal is a tool people choose over RViz 2, Rerun and Foxglove Studio: as
capable, more friendly, less over-engineered.

## 2. Shape of the workspace

25 crates. The ones that matter, roughly in dependency order:

| Crate | What it is |
| --- | --- |
| `rw-canonical` | The neutral value/schema model everything speaks. `CanonicalValue`, `Dialect`, `VisualizationRole`, `SchemaId`. |
| `rw-wire`, `rw-ws` | Framing, and the cross-platform WebSocket client. |
| `rw-codec-{cdr,json,rosmsg}` | Wire decoding. CDR is ROS 2's format and is already solved. |
| `rw-schema-{ros1,ros2,foxglove}` | `.msg` / IDL / JSON-schema parsing into `ParsedSchema`. |
| `rw-core` | Domain (`Request`, `Collection`, `Dashboard`, `HistoryEntry`), storage (SQLite + IndexedDB), the schema registry and its parser. |
| `rw-transport` | The `Transport` trait: connect, discovery, subscribe, publish, call, send goal. |
| `rw-transport-{dummy,foxglove-ws,rosbridge,replay}` | Its implementations. `dummy` is a synthetic robot and is what every screenshot scenario drives. |
| `rw-pipeline` | `CanonicalPipeline` — one upstream subscription per (connection, topic), ref-counted fan-out, rate/bandwidth meters, per-connection schema map. |
| `rw-record` | Recording and `Cursor`, the model for any timeline work. |
| `rw-tf` | The transform buffer: quaternions, slerp, interpolation, refused extrapolation. Pure, no GPU, no UI. |
| `rw-assets` | URDF/COLLADA/OBJ loading, forward kinematics, primitive shapes, the shipped robot catalog. |
| `rw-render` | wgpu. Layered `Scene`, point/line/lit-triangle pipelines, a geometry cache keyed by `Solid.key`. |
| `rw-ui` | Everything on screen. GPUI + gpui-component. |
| `rw-desktop`, `rw-web` | The two shells. |
| `xtask` | Build tasks and the screenshot harness. |

**Data flow.** Transport → `Frame { timestamp_ns, schema, value, raw }` →
pipeline (meters, fan-out) → a UI panel's `incoming` mutex → render.

## 3. What works today

Verified on screen, not just compiled.

- **Connections** — dummy, Foxglove WS, rosbridge, replay; several at once.
- **Requests** — topic (subscribe), service (call), action (goal with feedback),
  parameters (read/write). Collections and folders, rename, duplicate, move,
  delete, import/export, an unsaved-changes indicator, `ctrl+s`.
- **Views of a response** — Pretty (the request form read back), Raw, Visualize,
  Plot, Diff (freeze and watch what moves), History.
- **Message definitions** — click the Schema chip for the whole bundle as *this*
  connection sent it, with its registry hash. `ros2 interface show`, but for the
  robot in front of you rather than for whatever is installed locally.
- **History** — service and action runs kept in storage; clicking one puts its
  arguments back in the form.
- **Dashboards** — named, saved, their own split dock, panes pointed at topics,
  drag a topic in from the sidebar.
- **3D world** — TF-resolved layers: point clouds, laser scans, markers, paths,
  poses, the TF tree itself, URDF robots from the catalog, **and the robot the
  system itself describes on `/robot_description`**. A fixed-frame selector. A
  layer that will not resolve is dimmed and says why.
- **Record and replay** — record live topics, save, reopen, replay as a
  connection, and a transport bar to play, pause, scrub, re-speed and loop it.
- **Console** — the app's own notices and the robot's `/rosout`, one ordering.
- **Transforms** — the frame tree per system, with each edge's age, as a tab
  beside the console. `view_frames` and `tf_monitor` without leaving the app.
- **Toasts** — connection drops and failures only.
- **Settings** — a dialog with a rail of six sections; every value reaches
  something that reads it, live.
- **Command palette**, searchable topic picker, topic Hz/bandwidth/latency.

## 4. Hard-won facts

Things that cost time to learn. Do not re-derive them.

### The dock (gpui-component, pinned at `7acfc18`)

Paths are inside `~/.cargo/git/checkouts/gpui-component-*/7acfc18/`.

- **A bare `DockItem::tabs` is un-splittable.** `TabPanel::is_locked()` is true
  when `stack_panel.is_none()`; `draggable = !is_locked && !is_last_panel`,
  `droppable = !is_locked`. That is used deliberately: request tabs are handed
  to `set_center` bare so they cannot be split, while dashboards wrap theirs in
  `v_split` so they can.
- **The `DockItem` tree never learns about user splits.** A panel dragged into a
  new pane becomes invisible to `add_panel`/`remove_panel`. `Panel::on_added_to`
  plus `docking::Home` is how panels keep track of where they actually live.
- **The dock fixes its own tab variant** (`tab_panel.rs:876`, default
  `TabVariant::Tab`, radius hard-coded to 0). `TabBar` has `.pill()`,
  `.segmented()` etc; `TabPanel` exposes no way to pass one. Only theme tokens
  reach it. Pill-shaped dock tabs need a library change.
- **A pane cannot be an inset rounded card in the split dock.** The `TabPanel`
  container has no border and no radius (`tab_panel.rs:1494`) and the resize
  handle is 1px. The one layout that draws each pane as a bordered rounded card
  with its strip inside is `Tiles` (`tiles.rs:1033-1042`), via
  `DockItem::tiles` — a trade against the split dock that has not been made.
- **`Panel::title_style`** (`panel.rs:74`) sets the strip's colours but only on
  the single-panel path (`tab_panel.rs:766`) — which is every dashboard pane.
  Untried, and the obvious next thing for the header-in-the-card problem.
- **Reading the `TabPanel` from inside `Panel::title` is a double lease** and
  panics. Use `Panel::set_active`, which the dock dispatches outside its update.
- Notifications: the layer is rendered by `Root::render_notification_layer`;
  raise one with `window.push_notification(note, cx)` (`WindowExt`).

### The screenshot harness

`cargo xtask screenshot-native <name> --out <dir>`; scenarios in
`xtask/scenarios/*.txt`. Steps: `move`, `click`, `rightclick`, `drag`,
`dragover`, `release`, `type`, `key`, `scroll`, `wait`, `shot`, `restart`.

- **A scenario that runs to completion proves nothing.** It silently clicks the
  wrong thing. Three scenarios were quietly lying for weeks — `service` was
  proving 0+22, `action` was computing Fibonacci of an unfilled order. Always
  open the PNGs.
- **Captures go stale.** GPUI paints on demand and `import` reads whatever was
  last composited, so a shot straight after typing can show the previous frame.
  Nudge the pointer (`move`) before the shot.
- **`InputEvent::Focus`/`Blur` never fire** — there is no window manager.
- **`type` appends**, it does not replace. Nothing asks for a backspace.
- **Any layout change drifts every coordinate below it.** Re-aim, do not leave
  broken.
- **Prefer keybindings to buttons whose position moves.** The save check sits
  left of the run button, so its x depends on the connection name's width;
  `dragdrop` uses `ctrl+s` instead.
- **Never run two scenarios at once.** They share one Xvfb display, so a second
  run's clicks land in the first run's window. A whole suite came back with
  most scenarios silently failing at the connection form because a single
  scenario was started beside it. Both runs exit 0.
- **Every shot in the run shares one output directory, so two scenarios naming
  a shot the same thing means one silently overwrites the other.** `param` was
  eating `dragdrop`'s `04-dropped` this way. Before adding a shot:

  ```
  grep -H "^shot " xtask/scenarios/*.txt | sed 's|xtask/scenarios/||' \
    | awk '{split($0,a,":shot "); print a[2], a[1]}' | sort \
    | awk '{if ($1==prev) print "COLLISION:", $1, prevfile, $2; prev=$1; prevfile=$2}'
  ```
- **Comparing two runs needs a metric, not `cmp`.** Half the shots hold live
  data. `compare -metric AE old/x.png new/x.png null:` sorted descending puts
  the real regressions at the top; anything animated (clouds, camera frames,
  feedback counts) is expected to move.

### Rendering and assets

- wgpu 29 (the version gpui resolves). Offscreen render → `copy_texture_to_buffer`
  → map → `RenderImage` → `window.paint_image`, with `window.drop_image(previous)`
  paired on every swap or the atlas grows at frame rate. 256-byte row alignment
  must be unpadded.
- URDF fixed-axis RPY composes as Rz·Ry·Rx. COLLADA `<matrix>` is row-major and
  carries export units.
- `Scene` is layered: each `Layer` carries the matrix that places it in the
  fixed frame, applied at draw time, so a moving robot costs a matrix per frame
  rather than a re-upload.

### ROS

- **A definition arrives concatenated**: the root, then a rule of `=` and
  `MSG: pkg/Type` for each type it references. `parser::split_bundle` handles it.
  ROS 1's connection header, rosbridge's `get_message_details` and Foxglove's
  schema field all use this shape.
- **A schema name is not an identity.** ROS 1's `std_msgs/Header` has `seq` and
  ROS 2's does not; the registry holds both whenever two robots are connected.
  Resolve by hash — `pipeline.schema_hash(connection, target)`.
- **ROS 2 does not send message definitions over the wire.** RViz only knows
  types because it is compiled against them. Definitions would have to come from
  a local install's ament index.
- Neither native transport works in a browser (no raw TCP, no UDP multicast), so
  `rw-web` keeps the bridges regardless.

## 5. Design rulings

From review. These are not preferences and should not be re-litigated.

- **Cards, not flat.** A pane is a card. Flattening panes into one surface with
  hairline rules was rejected outright.
- **Dashboards stay resizable, draggable, customisable.** That is their point.
- **Use gpui-component. Do not reinvent.** A hand-rolled chip inside the dock's
  own tab was rejected on sight.
- **No nested boxes.** Controls float on the pane; no tinted band behind a
  control that is already bordered.
- **No submenus.** Flat menus only.
- **No settings tree** in a pane. The handful of controls that matter, on the
  flat menu the dock already draws.
- **Nothing said twice.** No duplicated titles, no schema named in two places.
- **No section for something with no content** — and no chrome row that carries
  no information.
- **A topic is a subscription.** No publishing, and therefore no message form on
  a topic request.
- **Pretty is the default view.** "Tabs" means the request tabs across the top,
  not the view strip inside a response.
- Every pane uses `tokens::card` and its content fills the pane.

- **A setting nothing reads is worse than no setting.** Every value in the
  dialog is wired to its consumer, and the two that cannot be re-read per frame
  — the `/tf` subscriptions and the rate meters — are told about the change
  instead (`TfStore::resettle`, `CanonicalPipeline::set_rate_window_ns`).
- **`Settings` is a GPUI global**, because the numbers are read deep inside
  decode and render paths that have a `cx` and nothing else. The preferences
  file is only where it comes back from next launch.
- **Values that a transport callback needs are captured at subscribe time**
  (`series::Limits`, the point budget): a callback has the frame and nothing
  else, and reaching for a global from off the UI thread is not available.

- **Nothing gets hand-rolled that a dependency already does.** `ActionGoalId`
  was a second `Uuid`, two crates carried identical hex encoders, two carried
  identical 4×4 matrix code, and two transports carried a `spawn_task` worse
  than the shared one. Before writing a helper, check what is already in
  `Cargo.toml` — and before adding a dependency for sixteen hex digits, don't.
- **A `Vec` is not a ring.** Dropping the oldest with `remove(0)` shifts the
  whole buffer. `VecDeque` is the std type for a bounded history and both the
  plot series and the console line buffer use it.
- **Shared code goes in the crate with no dependencies.** `Mat4` lives in
  `rw-tf` so that `rw-render` and `rw-assets` can both reach it without either
  reaching the other: an asset loader does not want wgpu and a renderer does
  not want a URDF parser.

## 6. Open issues

Ranked by what it costs you.

1. **The browser build has never been seen running.** It compiles again (it did
   not, for some time — see `docs/web-known-issues.md`), but headless Chromium
   here has no WebGPU, so it stops at the loading screen and nothing about it
   is verified. The biggest unknown in the project.
2. **This app burns ~0.7% CPU drawing nothing.** Found by benchmarking, not by
   looking. With no connection and no subscription the welcome screen should
   cost nothing; something is repainting. Cheap to chase and it is the one
   runtime number the Tauri app wins on its merits.
3. **The pane header floats outside its card.** `/dummy/counter · 165 · 9.44 Hz ·
   ⋯` sits on the grey with the card starting beneath it. Asked for, not
   delivered — blocked on `Panel::title_style` or a decision about `Tiles`.
   See §4.
4. **Parameter history is recorded nowhere visible** — so it is not recorded at
   all. Parameters have runs, but the parameter form is its own response and
   there is no response card to hang a History tab on. Needs somewhere on the
   PARAMETERS card first.
5. **The drop target is a full-pane solid wash.** Heavy; a border or light tint
   would read better.
6. **The world pane's layer rail** is 240px of full-height card for two rows.
7. **Marker types 9 (text) and 10 (mesh resource) are not decoded.** Text needs
    glyph rendering, which `rw-render` does not have.
8. **`/dummy/points` and `/dummy/image` build no payload form** — the schema
    does not reach `message_for` from the registry. Harmless now that topics do
    not publish, but the same gap would bite a service with those types.
9. **Settings live only in a dialog.** The owner asked for "dialog now, panel
    later"; the panel is not built. The content is a plain `v_flex` of rows, so
    moving it into a dock panel is a wrapper change, not a rewrite.
10. **Three settings sections are thin.** Requests holds one row, Console holds
    one. `marker::LIST_BUDGET`, `tree::MAX_CHILDREN`, the console's default
    level filter and a request's default view are all still constants.

## 6a. Parity with the Tauri app

The thing this replaces is on `main`: SvelteKit + Tauri 2, with three.js,
uplot and urdf-loader in the browser. **The Rust core is the same code** —
`rw-core`, `rw-canonical`, `rw-transport*`, `rw-pipeline`, the codecs and the
schema crates were lifted across unchanged. What differs is everything above
the pipeline: a webview and three.js there, GPUI and wgpu here.

**Reached, feature for feature:** requests of all four kinds with schema-driven
forms and autocomplete; connections; dashboards with splits, groups and
per-pane settings; image, plot, point-cloud and raw panes; the live field
tree; settings; sidebar, tabs, status bar and footer.

**Only here:** the 3D world with TF-resolved layers, the transform tree panel,
record and replay with a transport bar, request history, the diff view, the
message-definition viewer, the command palette, connection toasts, and
`/robot_description`.

**Only there — the one real gap:**

- **Joint articulation.** `robotkit/jointDriver.ts` exposes
  `setJoint`/`applyNamedPositions`, wired to a `JointControlsOverlay` of
  sliders and to `sensor_msgs/JointState`. Here `world.rs` solves the
  description once at `Pose::rest` and lets `/tf` place every link. For a
  robot running `robot_state_publisher` that is the better answer — the tree
  is the truth and the rest pose is only the fallback — but a robot that
  publishes `/joint_states` and no `/tf` stands still here and moves there,
  and there is no way to pose a description by hand at all.
- `PointInspector`, the worked example of a third-party pane. There is no
  extension point for one here.

So: at parity except for joint articulation. Anything that claims otherwise
should be checked against `src/lib/robotkit/` on `main` before it is believed.

### Measured against it

`docs/benchmarks.md` has the numbers and the method. In short: 2.8× faster to
a mapped window, 3.1× less resident memory, one process instead of three, one
toolchain instead of two, no system webview — and a bigger binary and a slower
build. The under-load rendering comparison is **not** done: synthetic clicks
are swallowed by the WebKit view under Xvfb with no window manager, so there is
no honest number for it yet.

Two traps that cost time and would cost it again:

- **A plain `cargo build --release` on the Tauri package does not build the
  app.** It produces a binary pointing at `devUrl`, which renders
  `Could not connect to localhost`. The first set of numbers measured that.
  Build it with `bunx tauri build --no-bundle`, and *look at a screenshot*
  before believing anything.
- **Measure the whole process tree.** Tauri renders in separate
  `WebKitWebProcess` children; the launched PID accounts for almost none of it.

## 7. Frozen / out of scope

Decided, not forgotten.

- **Native ROS 1 (TCPROS) and ROS 2 (DDS/RTPS) transports.** Explicitly frozen.
  No `rclrs`, `rustdds` or `dust-dds` in the workspace.
- **MCAP.** Deferred.
- **Request descriptions.** Ruled out.
- **Environment variables** (`{{ns}}/scan`). Considered, not chosen.
- **The occupancy grid.** Dropped by the owner. `nav_msgs/OccupancyGrid` ships a
  schema and has no `VisualizationRole` and no decoder, so a `/map` renders as a
  field table; it stays that way. It would have needed a texture pipeline in
  `rw-render` that does not exist, and that pipeline is not being built for it.

## 8. Verification

All three green before every commit. Never skip or `#[ignore]` a test to get
there.

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # 823 passing, 1 ignored
```

The ignored test is `rw-transport-rosbridge/tests/live_action.rs`; it needs a
real rosbridge and predates all of this.

Then on screen:

```
cargo xtask list-scenarios
cargo xtask screenshot-native <name> --out target/shots
cargo xtask screenshot-native --out target/shots        # all 24, 102 shots
```

**Open the PNGs.** Then compare against the previous run's directory — a shot
that moved is either live data or a regression, and the difference matters.
Note that `target/` does not survive a container restart, so keep a known-good
run's path in the working notes when it matters.

The wasm path is not covered by `--workspace`:

```
cargo check -p rw-core --target wasm32-unknown-unknown \
      --no-default-features --features wasm-storage
rustup run nightly cargo build -p rw-web --target wasm32-unknown-unknown
```

The second is the one that matters and the one that was missing: `rw-core`
alone compiled for wasm all the while `rw-web` did not.

## 9. Plan

Done, in order: TF and the 3D world · freeze & diff · parameters · topic stats ·
log console · searchable picker · drag-and-drop · dashboards · record/replay ·
schema resolution per connection · toasts · history · settings.

**Settings, delivered.** Eight values, each one a constant nobody could reach
before, now in a dialog and wired to what reads it:

| Setting | Default | Reaches |
| --- | --- | --- |
| `history_depth` | 50 | read where a run is recorded, so lowering it bites on the next call |
| `console_lines` | 2000 | read on every line pushed |
| `point_budget` | 400 000 | threaded through `viz::draw` → `cloud::decode` |
| `plot_window` | 600 | `series::Limits`, captured at subscribe time |
| `plot_fields` | 12 | same |
| `follow_transforms` | on | a live switch — off drops the `/tf` subscriptions *and* the trees they filled |
| `tf_window_secs` | 10 | `Buffer::set_window`, on the buffers already running |
| `rate_window_secs` | 5 | `Meter::set_window`, on the meters already running |

Every default is the old constant, which is what makes the unchanged
screenshots the regression test for the whole feature.

**Next, in rough order of value:**

1. **The 0.7% idle CPU** (§6.1). Found by benchmarking. Small, and it is the
   one runtime number the old app beats us on fairly.
2. **The pane header** (§6.2) — asked for, still floating outside its card, and
   the oldest thing on this list.
3. **The under-load benchmark** — the comparison that is still missing, and the
   one that would say the most. Needs a window manager or Tauri's WebDriver
   harness; see `docs/benchmarks.md`.
4. **Joint articulation** (§6a) — the one feature the Tauri app has and this
   does not.
5. **The drop-target wash and the layer rail** (§6.4, §6.5) — both are visual
   debt already written down and both are an afternoon.

**Delivered since the settings pass:**

- The transport bar — play, pause, scrub, speed, loop on a recording.
- The rate fix — `stats.rs` divided by `n` where `ros2 topic hz` divides by
  `n−1`, reading a quarter high at 1 Hz. Bandwidth had the same off-by-one and
  now equals rate times mean message size.
- `/robot_description` — the world pane draws the robot the system says it is,
  not only the seven that ship here.
- The transform tree, as a tab beside the console.
