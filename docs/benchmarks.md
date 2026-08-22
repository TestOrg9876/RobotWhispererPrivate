# GPUI vs the Tauri app: what was actually measured

The app this replaces is on `main`: SvelteKit + Tauri 2, three.js, uplot,
urdf-loader. **The Rust core is the same code on both sides** — `rw-core`,
`rw-canonical`, `rw-transport*`, `rw-pipeline`, the codecs and the schema
crates were carried across unchanged. What differs is everything above the
pipeline, so this compares UI layers, not ROS plumbing.

Reproduce with `scripts/bench/measure.sh`. Every number below is the median of
five runs.

## Method, and why each control is there

- **Both windows pinned to 1440×900.** The Tauri app opens at 800×600 and this
  one at 1440×900. Per-pixel cost is most of what is being compared, so
  leaving that alone would have handed Tauri a 2.7× head start.
- **Every number summed over the whole process tree.** Tauri runs its UI in
  separate `WebKitWebProcess` and `WebKitNetworkProcess` children. Reading only
  the launched PID would credit it with none of its own rendering.
- **Same machine, same X server, one app at a time.** Two apps on one Xvfb
  display corrupt each other's input.
- **The Tauri app was built the way it ships** (`tauri build --no-bundle`).
  A plain `cargo build --release` on that package produces a binary that points
  at the dev server and renders `Could not connect to localhost` — the first
  set of numbers taken here measured exactly that and were thrown away. The
  screenshot is what caught it.

## Runtime

| | GPUI | Tauri | |
| --- | ---: | ---: | --- |
| Cold start to mapped window | **0.063 s** | 0.176 s | 2.8× faster |
| Resident memory, idle | **174 MB** | 543 MB | 3.1× less |
| Processes | **1** | 3 | |
| CPU, idle | 0.7 % | ~223 % | see below |

**The idle CPU number needs its caveat.** Neither app had GPU acceleration
here — both ran on llvmpipe under Xvfb — and the Tauri welcome screen animates
its logo with a `requestAnimationFrame` loop plus CSS keyframes
(`AnimatedBot.svelte`). Two cores of software rasterisation is what that costs
in this environment; on a desktop with GPU compositing it would be far lower.
The honest reading is not "300× better" but: *an animated welcome screen is
free in a retained-mode GPU renderer and expensive in a webview without
compositing.* The memory and startup figures carry no such caveat.

**0.7 % is also not zero**, and this app is drawing nothing at that point.
That is worth chasing — see the open issues.

## Artifacts

| | GPUI | Tauri |
| --- | ---: | ---: |
| Binary, stripped | 48.7 MB | 17.6 MB |
| Frontend assets | — | 12 MB |
| System webview required | none | 121 MB (webkit2gtk) |
| **Total to install** | **48.7 MB** | 29.6 MB + webkit |

Tauri's binary is smaller and its shipped artifact is smaller. It is only
smaller because the renderer is not in it: webkit2gtk had to be installed on
this machine before the app would build or run at all, and the version it
finds is the version the UI gets. The GPUI binary carries its renderer and
depends on nothing but X11 and Vulkan.

Our own release profile keeps line tables (`debug = "line-tables-only"`), which
is why the unstripped binary is 345 MB. The 48.7 MB above is stripped, which is
the like-for-like figure.

## Build and dependencies

| | GPUI | Tauri |
| --- | ---: | ---: |
| Clean release build | 9 m 51 s | 6 m 07 s (5 m 19 s Rust + 48 s Vite) |
| Rust crates in the lockfile | 910 | 556 |
| npm packages | 0 | 364 |
| Toolchains needed | cargo | cargo + bun/node |

We build slower and pull more Rust crates — gpui, wgpu and their graph are not
cheap. Counting both ecosystems the totals are near enough identical (910 vs
920); the difference is that one of them is a single toolchain.

## What could not be measured

**Rendering under load** — a point cloud streaming into both — is the number
that would matter most, and it is missing. Driving the Tauri UI needs synthetic
clicks into the WebKit view, and under Xvfb with no window manager those are
swallowed: verified by screenshot, the app stayed on its welcome screen through
every attempt. Rather than report a number from a run that did not do what it
claimed, there is no number here. It needs a session with a window manager, or
Tauri's own WebDriver harness.

## Honest summary

Faster to start, a third of the memory, one process instead of three, one
toolchain instead of two, and no system webview to depend on. Bigger binary,
slower build. The load comparison is unfinished.
