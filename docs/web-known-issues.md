# What does not work in the browser build

The wasm target builds, starts, renders the whole shell and talks to a robot.
Mouse-driven work is complete: connections, request tabs, the target field's
discovery offers, the response views. What follows is what is broken and why,
so nobody re-derives it.

## Keyboard actions panic inside GPUI's profiler

**Symptom.** Pressing a key that resolves to a GPUI *action* — Enter in a text
field, or any of the app's shortcuts (Ctrl-P, Ctrl-S, Ctrl-B…) — panics with:

```
panicked at library/std/src/sys/time/unsupported.rs:13:9:
time not implemented on this platform
```

immediately followed by `RefCell already borrowed` from
`gpui/src/app/async_context.rs`. The second panic is the damage: the first
unwinds while GPUI holds a borrow of its app state, and after that the window
stops responding to events. Typing plain characters and clicking are unaffected,
because neither dispatches an action.

**Cause.** `gpui::profiler::actions::update_running_action` runs on every action
dispatch and calls `std::time::Instant::now()`. On `wasm32-unknown-unknown`
`std::time::Instant` has no backing clock and panics. The rest of GPUI does not
have this problem because it uses `scheduler::Instant`, which is `web_time`'s —
`crates/gpui/src/profiler/actions.rs` is the one file that imports std's
instead. The function is behind `#[cfg(feature = "profiler")]`, and
`gpui-component` enables `gpui/profiler` for every target.

**Why it is not fixed here.** Cargo features are additive: a crate cannot turn
off a feature another crate in the graph switches on. Fixing it needs one of

- `gpui` to import `scheduler::Instant` in `profiler/actions.rs` (a one-line
  upstream change, and the obviously correct one), or
- `gpui-component` to stop requiring `gpui/profiler`, or
- a `[patch]` pointing at a fork of one of them.

The revision this repository pins and the newest upstream revision both still
have it, so it is worth reporting rather than waiting out.

## Things that were fixed, and are worth not reintroducing

- **GPUI's `BackgroundExecutor::timer` cannot be used on wasm** for anything on
  the render path — see `rw-ui/src/tick.rs`. It stamps timers with
  `Instant::now()`. Every wait in this app goes through `tick::sleep`.
- **The `instant` crate needs its `wasm-bindgen` feature.** Without it, it falls
  back to `std::time::Instant` — the same panic. It sits under
  gpui-component's undo manager and scrollbars. `rw-ui` declares `instant` for
  the wasm target purely to switch that feature on.

## Finding the next one

The browser harness reports which scenario step a panic arrived during, and
raises Chrome's stack limit past the ten frames a wasm panic spends on the panic
machinery. A release wasm inlines the frames above a panic away, so run
`cargo xtask screenshot-web <scenario> --dev` when a stack ends somewhere
useless — the debug module is far larger and slower to load, and it names the
caller.
