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
- **History** — service and action runs kept in storage; clicking one puts its
  arguments back in the form.
- **Dashboards** — named, saved, their own split dock, panes pointed at topics,
  drag a topic in from the sidebar.
- **3D world** — TF-resolved layers: point clouds, laser scans, markers, paths,
  poses, the TF tree itself, and URDF robots from the catalog. A fixed-frame
  selector. A layer that will not resolve is dimmed and says why.
- **Record and replay** — record live topics, save, reopen, replay as a
  connection.
- **Console** — the app's own notices and the robot's `/rosout`, one ordering.
- **Toasts** — connection drops and failures only.
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

## 6. Open issues

Ranked by what it costs you.

1. **The pane header floats outside its card.** `/dummy/counter · 165 · 9.44 Hz ·
   ⋯` sits on the grey with the card starting beneath it. Asked for, not
   delivered — blocked on `Panel::title_style` or a decision about `Tiles`.
   See §4.
2. **`robot_description` is never read.** The world pane loads URDFs from a
   shipped catalog of 7, so pointing this at a real robot shows its scan and its
   path and no machine. RViz's most basic behaviour.
3. **Replay has no transport controls.** `ReplayTransport` has `set_playing`,
   `set_speed`, `set_looping`, `seek` and a `progress()` channel, all tested,
   with **zero UI callers**. Best effort-to-value ratio in the repo.
4. **No occupancy grid.** `nav_msgs/OccupancyGrid` ships a schema but has no
   `VisualizationRole` and no decoder, so a map renders as a field table. Needs
   a texture pipeline in `rw-render`, which does not exist yet.
5. **No TF tree view.** `Buffer::tree()` returns exactly the shape a view wants
   (`frame`, `parent`, `depth`, `is_static`, `samples`, `newest_ns`) and its only
   caller picks the root out and throws the rest away.
6. **Parameter history is recorded nowhere visible** — so it is not recorded at
   all. Parameters have runs, but the parameter form is its own response and
   there is no response card to hang a History tab on. Needs somewhere on the
   PARAMETERS card first.
7. **The drop target is a full-pane solid wash.** Heavy; a border or light tint
   would read better.
8. **Hz is biased high.** `stats.rs` divides by `live.len()`; `ros2 topic hz`
   divides by `n−1`. 0.2% at 100 Hz, ~10–25% at 1 Hz.
9. **The world pane's layer rail** is 240px of full-height card for two rows.
10. **Marker types 9 (text) and 10 (mesh resource) are not decoded.** Text needs
    glyph rendering, which `rw-render` does not have.
11. **`/dummy/points` and `/dummy/image` build no payload form** — the schema
    does not reach `message_for` from the registry. Harmless now that topics do
    not publish, but the same gap would bite a service with those types.

## 7. Frozen / out of scope

Decided, not forgotten.

- **Native ROS 1 (TCPROS) and ROS 2 (DDS/RTPS) transports.** Explicitly frozen.
  No `rclrs`, `rustdds` or `dust-dds` in the workspace.
- **MCAP.** Deferred.
- **Request descriptions.** Ruled out.
- **Environment variables** (`{{ns}}/scan`). Considered, not chosen.

## 8. Verification

All three green before every commit. Never skip or `#[ignore]` a test to get
there.

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # 791 passing, 1 ignored
```

The ignored test is `rw-transport-rosbridge/tests/live_action.rs`; it needs a
real rosbridge and predates all of this.

Then on screen:

```
cargo xtask list-scenarios
cargo xtask screenshot-native <name> --out target/shots
cargo xtask screenshot-native --out target/shots        # all 20, 89 shots
```

**Open the PNGs.** Then compare against the previous run's directory — a shot
that moved is either live data or a regression, and the difference matters.
Note that `target/` does not survive a container restart, so keep a known-good
run's path in the working notes when it matters.

The wasm path is not covered by `--workspace`:

```
cargo check -p rw-core --target wasm32-unknown-unknown \
      --no-default-features --features wasm-storage
```

## 9. Plan

Done, in order: TF and the 3D world · freeze & diff · parameters · topic stats ·
log console · searchable picker · drag-and-drop · dashboards · record/replay ·
schema resolution per connection · toasts · history.

**Next: settings.** Today `Settings` is a 460px modal holding a theme list and a
version string. It becomes a left rail of sections and a content pane — content
first, in the dialog; the move to a dock panel is a separate change. The values
already exist as constants nobody can reach:

| Section | From |
| --- | --- |
| Appearance | theme (already there) |
| Data | `cloud::BUDGET` 400k, `marker::LIST_BUDGET`, `tree::MAX_CHILDREN` |
| Plots | `series::WINDOW` 600, `series::MAX_FIELDS` 12 |
| Rates | `stats::WINDOW_NS` 5 s |
| Transforms | `rw_tf::DEFAULT_WINDOW_NS` 10 s, and whether `/tf` is auto-subscribed |
| Console | retention, default level filter |
| Requests | default view, `HISTORY_CAP` 50 |

`prefs.rs` grows a `settings: Settings` beside `theme` and `layout`, same
serde-defaulted pattern so an older preferences file still loads. Each default
is today's constant, and the unchanged screenshots are what proves it.

After that, in rough order of value: replay transport controls (§6.3),
`robot_description` (§6.2), the pane header (§6.1).
