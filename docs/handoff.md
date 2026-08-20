# Handoff

For whoever picks this up next. `docs/roadmap.md` is the plan and it is still
the plan; this is the part that is not in it — what has landed since, what the
owner has ruled on in review, and the walls worth knowing about before you hit
them yourself.

## Where you are

- **Branch: `claude/gpui-project-rewrite-ahwvje`.** Develop, commit and push
  there. Note that a session's system prompt may designate a different branch
  (it designated `claude/new-session-1soyuo` for the session that wrote this);
  `main` is the old Svelte/Tauri app and holds none of this code. The Rust
  workspace only exists on this branch.
- No pull request has been opened, deliberately.
- Working tree is clean, everything is pushed.

## Verifying

All three, green, before every commit. Never disable, skip or `#[ignore]` a
test to get there.

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # 762 passing, 1 ignored
```

The one ignored test is `crates/rw-transport-rosbridge/tests/live_action.rs`
— it needs a real rosbridge. It was ignored before any of this work.

Then prove it on screen:

```
cargo xtask list-scenarios
cargo xtask screenshot-native <name> --out target/shots
cargo xtask screenshot-native --out target/shots        # all 19
```

`cargo xtask` works because `.cargo/config.toml` aliases it. That alias was
added by this work: `docs/roadmap.md:244` and `docs/web-known-issues.md:58`
both told you to run `cargo xtask …` and it was not a command — CI uses
`cargo run -p xtask --` (`.github/workflows/ci.yml:46`).

**Open the PNGs and look at them.** Several of the mistakes below were caught
only by looking.

## What landed

Nine commits, oldest first:

| Commit | What |
| --- | --- |
| `4c4c7a9` | `RequestKind::Param` — read and write a node's parameters |
| `c4f761b` | Drop a topic from the sidebar onto a live pane |
| `f822534` | The 3D view's corners are clipped properly |
| `dc679e2`, `6131bd7` | Request tabs as chips — **reverted, see below** |
| `d932a22` | The request bar floats; tabs back to the dock's own |
| `5f2b2d4`, `175a9ef`, `b56028d` | The Pretty view, and arrays as a row per element |

With `4c4c7a9` and `c4f761b`, every item in `docs/roadmap.md` — Phase 1 (TF and
one 3D world), Phase 2 (freeze & diff), Phase 3 (parameters, topic stats, log
console, searchable picker, drag-and-drop) — is delivered. Everything after
that came out of the owner's review, not the roadmap.

### Parameters (`4c4c7a9`)

`crates/rw-ui/src/param.rs` is the whole protocol, pure and tested: the three
`rcl_interfaces` service names, the ten `ParameterType` codes, an encoder that
fills *every* field of a `ParameterValue`, and a decoder that refuses a
response of the wrong length rather than pairing values with the wrong names.
`crates/rw-transport-dummy/src/params.rs` is `/dummy/planner`, a node with nine
parameters that refuses an undeclared name and a wrong-typed value; its encoder
is hand-written rather than shared with the decoder, because the two agreeing
is the thing worth checking.

Two invariants that are easy to break:

- A write uses **the kind the parameter was declared with**, not the kind the
  typed text parses into. A node refuses a `double` where it declared an
  `integer`.
- A parameter the node has no value for (`NOT_SET`) has no declared type to
  honour, so there what was typed decides. Writing back the `NOT_SET` it was
  read as silently discards the value being set — that was a real bug, fixed,
  and tested on both sides.

`migration_7` widened the `requests.kind` CHECK. SQLite cannot widen one in
place, so the table is rebuilt: named columns, both indexes recreated. There
are tests in `migrations.rs` that build a database stopped at version 6, put a
row in it, and step it forward — the fresh-database tests never exercise that
path.

### The Pretty view (`5f2b2d4`, `b56028d`)

`crates/rw-ui/src/tree.rs`. The response laid out as the request form, read
back: same rows, same label column, same input boxes, disabled. `View::Pretty`
is first in `View::ALL` and is the default.

Two things hold it together and should stay:

- **One row builder.** `tokens::field_row(path, type_name, cx)` is used by both
  the request editor and this. The only difference between filling a message in
  and reading one out should be whether the box takes typing.
- **Boxes are kept, not rebuilt.** `TreeView::refill` compares the new row
  shape against the old; same shape means the existing `InputState`s get
  `set_value`, and only a change of shape or a fold builds new ones. A topic at
  100 Hz costs one `set_value` per *visible* leaf. Rows go through
  `uniform_list`, so off-screen rows build nothing. If you change this, keep
  both properties — a naive rebuild-per-message is an entity per leaf per
  frame.

Bounds worth keeping: `MAX_CHILDREN` (200) caps an open array, `FOLD_OVER` (16)
decides what arrives folded, `MAX_DEPTH` (24) guards a decoder gone wrong.
Folds are keyed on path, so a branch left open stays open as messages arrive.

### Arrays as rows (`175a9ef`)

`form::parse_list` and `form::rows_at` are the pure half;
`panels/request.rs::Inputs` is the state — `One(input)` or
`List { element, rows }`. `form::MAX_ROWS` (128) is the ceiling: past it the
field falls back to the single comma box, because that is a fallback for data,
not a second way to edit a list.

## What the owner has ruled on

These came out of review. They are not preferences.

- **Cards, not flat.** A pane is a card. An earlier pass flattened the panes
  into one continuous surface with hairline rules; it was rejected outright.
  It is reverted — do not redo it.
- **The pane's header row belongs to the card.** The `/dummy/counter · 167 ·
  9.56 Hz · ⋯` strip should be *inside* the pane's card, not floating above it.
  **This is still open.** See the wall below.
- **Dashboards stay resizable, draggable and customisable.** That is the point
  of them. Do not trade the split dock away for a layout that loses it.
- **Use gpui-component. Do not reinvent.** A hand-rolled chip drawn inside the
  dock's own tab was rejected on sight — see `dc679e2` and its revert
  `d932a22`.
- **No nested boxes.** The request bar is a bordered control; it had a tinted
  band behind it, which read as a box inside a box. Removed. Controls float on
  the pane.
- **"Tabs" means the request tabs** across the top — the ones you get with
  several requests open — not the Raw/Pretty/Visualize strip inside a response.
  Getting that wrong cost a round trip.
- **Pretty is the default view**, not Raw.
- The tab treatment the owner picked from gpui-component's five variants was
  **Segmented**, which is what the response strip already used.

And the standing rules from the roadmap, which still hold: flat menus, no
duplicated titles, no chrome row that carries no information, no settings tree,
no section for something with no content, every pane uses `tokens::card` and
its content fills the pane.

## Walls in gpui-component

Pinned at rev `7acfc184382d30864a688fdaa6c9ff719efc53ae`. Paths below are
inside `~/.cargo/git/checkouts/gpui-component-*/7acfc18/`.

- **The dock fixes its own tab variant.** `crates/ui/src/dock/tab_panel.rs:876`
  constructs `Tab::new()` with the default `TabVariant::Tab` — square, with
  left/right borders, notched into the rule under the strip. `TabBar` has
  `.pill()`, `.segmented()`, `.outline()`, `.underline()`, but `TabPanel`
  exposes no way to pass one. The only knobs from outside are theme tokens:
  `tab_bar`, `tab`, `tab_active`, `tab_foreground`, `tab_active_foreground`.
  Radius is hard-coded to 0 for that variant. Pill-shaped dock tabs need a
  library change, not a trick.
- **A pane cannot be an inset rounded card in the split dock.** The `TabPanel`
  container is `.size_full().overflow_hidden().bg(tokens.background)` with no
  border and no radius (`tab_panel.rs:1494`), and the resize handle between
  panes is `HANDLE_SIZE = px(1.)`
  (`crates/base/src/resizable/resize_handle.rs:12`). There is no margin for a
  card to sit in and nothing to round. The one layout that *does* draw each
  pane as a bordered, rounded card with its strip inside is `Tiles`
  (`crates/ui/src/dock/tiles.rs:1033-1042`), reachable as `DockItem::tiles`.
  Whether that is an acceptable trade against the split dock is the owner's
  call and has not been made.
- **`Panel::title_style`** (`crates/ui/src/dock/panel.rs:74`) sets the strip's
  background and foreground, but only on the single-panel path
  (`tab_panel.rs:766`) — which is every dashboard pane. Untried; it is the
  obvious next thing for the header-belongs-to-the-card problem.
- **Reading the `TabPanel` from inside `Panel::title` is a double lease.** The
  dock calls `title` from inside its own update, so
  `tab_panel.read(cx).active_panel(cx)` panics
  (`entity_map.rs:164 double_lease_panic`). Use `Panel::set_active`, which the
  dock dispatches *outside* its update precisely so a panel can record it
  (`tab_panel.rs:319-328`).

## Traps in the screenshot harness

- **Captures go stale.** GPUI paints on demand and `import` reads whatever the
  window last composited, so a shot straight after typing can show the previous
  frame. Nudge the pointer over something with a hover style — a button —
  before the shot. Between a freeze and a write the window sometimes stops
  recompositing entirely under Xvfb; `xtask/scenarios/param.txt` has a comment
  where a shot was dropped for that reason rather than shipping one that lies.
- **`key` and `type` do not both reliably land in the same field.** Scenarios
  append rather than replace; nothing asks for a backspace.
- **`InputEvent::Focus`/`Blur` never fire** — no window manager.
- **Any layout change drifts every click coordinate.** Re-aim the scenarios;
  do not leave them broken. A scenario that runs to completion is not proof —
  it silently clicks the wrong thing. Compare the shots.

## Open

- Task #14, the one the owner asked for and that is not done: **the pane's
  header row inside the pane's card.** Read the wall above first. `title_style`
  plus a decision about `Tiles` is where it stands.
- The old Svelte design (`git show origin/main:src/app.css`) is worth reading
  before more design work. `.tab-chip` at line 409 and `.pane-host` in
  `src/lib/dashboard/chrome/PaneHost.svelte` are the two the owner keeps
  pointing at. Its palette also gives parameters their own colour — pink, with
  actions in purple — where this app currently gives `RequestKind::Param`
  yellow (`tokens::kind_color`).
- Still explicitly out of scope: native ROS 1 (TCPROS) and ROS 2 (DDS)
  transports, and MCAP. No `rclrs`, `rustdds` or `dust-dds` in the workspace.
