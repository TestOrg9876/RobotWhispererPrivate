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

## Web

| | GPUI (wasm) | Tauri app (web target) |
| --- | ---: | ---: |
| Rust core as wasm | 29.9 MB | 2.5 MB |
| JavaScript / assets | 0.16 MB | 12 MB |
| **Total** | **30 MB** | **14.5 MB** |

Ours is roughly twice the download. The whole UI framework and renderer are in
that wasm; theirs ships a 2.5 MB core and does its drawing with three.js and
the browser's own layout engine, which are already there. Two caveats, both
against us and neither large enough to close the gap: our release profile keeps
line tables, and neither figure has been through `wasm-opt`, which their
shipping pipeline runs and ours does not.

**The browser build did not compile at all before this pass** — `Arc<dyn
Transport>` is not `Send` on wasm, where the trait is `?Send`, and two
`background_spawn` calls required it. Verified as pre-existing by checking out
`3a2d2a6`, before any of this session's work: nine errors there, nine here. The
fix is two lines — unsubscribing awaits a channel rather than doing work, so it
belongs on `spawn`.

It compiles now. **It does not boot in headless Chromium in this container**:
the page loads and stops at "Loading Robot Whisperer...". GPUI's web backend
needs WebGPU, which is not available here even with `--enable-unsafe-webgpu`
and swiftshader, so whether the app or the environment is at fault is
undetermined. It is not claimed as working.

## Throughput

`cargo xtask load-bridge` is a synthetic Foxglove/rosbridge server;
`scripts/bench/` drives both clients to a live subscription and measures the
process tree while it streams.

Every row was verified by screenshot: both clients subscribed, schema resolved,
messages counted. The bridge reports what it actually wrote each second, so a
run it could not keep up with cannot be mistaken for a client that kept up.

| load | offered | delivered | GPUI | Tauri |
| --- | --- | --- | --- | --- |
| welcome screen | — | — | **0.9 % / 204 MB** | 222 % / 543 MB |
| request tab, no subscription | — | — | **0.9 % / 209 MB** | — |
| `Float64` @ 1000 Hz | 1000 msg/s | 991 msg/s | 153 % / **213 MB** | **120 %** / 611 MB |
| 5 topics @ 200 Hz | 1000 msg/s | 996 msg/s | 142 % / **216 MB** | **122 %** / 679 MB |
| `Float64` @ 100 Hz, rosbridge | 100 msg/s | 99 msg/s | 154 % / **213 MB** | **114 %** / 614 MB |
| `PointCloud2`, 60k points @ 10 Hz | 9.2 MiB/s | 9.2 MiB/s | **130 % / 703 MB** | 273 % / 1368 MB |
| `PointCloud2`, ROS 1 dialect | 9.2 MiB/s | 9.0 MiB/s | **131 % / 705 MB** | 271 % / 1379 MB |
| `CompressedImage`, 300 kB @ 60 Hz | 17.6 MiB/s | 17.6 MiB/s | **143 % / 369 MB** | 279 % / 959 MB |
| `Image`, 1080p rgb8 @ 30 Hz | 178 MiB/s | 168 MiB/s | **220 % / 5493 MB** | 294 % / 6989 MB |

### Where each side wins, and why

**On real robotics payloads we use less than half the CPU** — 130 % against
273 % on point clouds, 143 % against 279 % on compressed images — and about
half the memory. That is the case the app exists for.

**On trivial scalar messages the Tauri app wins**: 120 % against our 153 % at
1000 Hz. Saying otherwise would be picking the flattering rows.

### The CPU tracks the display rate, not the bandwidth

Ours, ordered by how fast the pane updates rather than by bytes:

| pane updates at | bandwidth | CPU |
| --- | --- | --- |
| 9.96 Hz (point cloud) | 9.6 MB/s | 130 % |
| 22.1 Hz (compressed image) | 6.8 MB/s | 143 % |
| 50 Hz × 5 panes | 700 B/s | 142 % |
| 58.1 Hz (scalar) | 697 B/s | 153 % |

CPU climbs with the redraw rate and ignores the bandwidth entirely — 9.6 MB/s
at 10 Hz is cheaper than 697 B/s at 58 Hz. **This is llvmpipe drawing the
window in software, not message processing**, which also explains the scalar
rows: nothing else is happening there, so redraw is all the cost there is. On a
GPU most of this column should collapse. It is an inference from the
correlation, not something measured directly — no GPU was available here.

### The rate cap, and where it is missing

`default_target_hz_for_schema` caps a subscription at 60 Hz by default, 30 Hz
for images, 15 Hz for point clouds, 200 Hz for `JointState` and `Imu`. The
Foxglove transport enforces it with `min_interval_ns` and drops the rest
*without decoding them* — 1000 Hz displays at 58 Hz, five topics at 50 Hz each,
a 60 Hz compressed image stream at 22 Hz. No view can show more, and decoding
what will never be drawn is waste.

**The rosbridge transport does not apply it.** A 100 Hz topic displayed at
98.8 Hz where Foxglove would have capped it at 60. Same policy, one transport,
so the same stream costs differently depending on how you connected. That is a
bug, not a design choice.

### 1080p raw: both clients fall behind

At 178 MiB/s neither keeps up. The delivered rate drops to 168 MiB/s — the only
row where the bridge's writes stall — and ours reports **2.8 seconds of
latency** at 14.8 Hz of an offered 30.

That backlog *is* the memory: **5.5 GB resident for us, 7.0 GB for the Tauri
app.** Frames arrive faster than they are drawn and queue without a bound.
Holding 1.5 GB less than the other one is not a defence; a client seconds
behind and growing has already failed. It is the most actionable thing this
exercise found and it is now the top open issue.

## What is still not measured

- **GPU.** Everything here is llvmpipe under Xvfb, which the section above
  argues is most of our CPU column. This is the biggest gap in the numbers.
- **What the Tauri app processes.** It reports no message count, rate,
  bandwidth or latency, so only its CPU and memory can be compared. Ours
  reports all four, which is why our side of every row can be checked against
  the bridge and theirs cannot.
- **Large payloads over rosbridge.** The JSON transport is covered for scalars
  only; point clouds and images over rosbridge would need base64 array encoding
  in the bridge.

## What could not be measured (first pass, superseded)

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
