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

The section the first pass could not produce. `cargo xtask load-bridge` is a
synthetic Foxglove/rosbridge server; `scripts/bench/` drives both clients to a
live subscription and measures the process tree while it streams.

Every row below was verified by screenshot: the client is subscribed, the
schema resolved, and messages counted. Runs where that check failed are listed
as failures rather than as numbers.

| load | offered | delivered | GPUI CPU / RSS | Tauri CPU / RSS |
| --- | --- | --- | --- | --- |
| welcome screen | — | — | **0.9 % / 204 MB** | 222 % / 543 MB |
| request tab, no subscription | — | — | **0.9 % / 209 MB** | — |
| 1 topic, `std_msgs/Float64` @ 1000 Hz | 1000 msg/s | 995 msg/s | 153 % / **213 MB** | 144 % / 619 MB |
| 5 topics @ 200 Hz each | 1000 msg/s | 996 msg/s | 143 % / **216 MB** | 142 % / 674 MB |
| 1080p `sensor_msgs/Image` rgb8 @ 30 Hz | 178 MiB/s | 178 MiB/s | **198 % / 5114 MB** | 295 % / 5720 MB |

### The number that changes how the rest reads

**This client throttles on purpose, at the transport, before decode.**
`default_target_hz_for_schema` caps a subscription at 60 Hz by default, 30 Hz
for images, 15 Hz for point clouds, 200 Hz for `JointState` and `Imu`; the
Foxglove transport enforces it with `min_interval_ns` and drops the rest
without decoding them.

So at 1000 msg/s offered it displays 58 Hz, and with five topics it displays
about 50 Hz on each — measured off the app's own rate readout, which matches
the cap. That is a design decision, not a shortfall: no view can show 1000 Hz,
and decoding what will never be drawn is waste.

It also means the CPU columns are **not equal work**. Ours decodes ~60
messages a second; the Tauri app has no rate cap and no rate readout, so what
it decodes cannot be read off the screen. Comparing 153 % against 144 % without
that caveat would be dishonest in our favour, and stating it is dishonest in
theirs — so: at equal *offered* load we use slightly more CPU and a third of
the memory, and we deliberately do less decoding.

### Where the difference is unambiguous

At 1080p rgb8 — 178 MiB/s, the heaviest load either client will meet — we use
**33 % less CPU** (198 % against 295 %) at the same delivered bandwidth. Both
clients read every byte off the socket: the bridge's writes never stalled, so
neither was applying backpressure.

**Both balloon to about 5 GB of RSS there, and that is a bug in both.** 178 MiB/s
against a client that draws 20-30 frames a second should not accumulate; ours
holds slightly less, which is not a defence. It is the most actionable thing
this whole exercise found.

### Memory, everywhere else

Three times less, consistently: ~215 MB against ~600 MB, in one process rather
than three. That gap is stable across every load and it is the clearest result
here.

## What is still not measured

- **Point cloud, compressed image, ROS 1 dialect, and rosbridge.** The harness
  encodes these wrongly — verified by screenshot: the client sat on "waiting
  for the first message" while the bridge delivered at it. Adding the schema
  dependency bundles fixed part of it and not all. Those cells are absent
  rather than filled with numbers from runs that did not do what they claimed.
- **What the Tauri app processes.** It shows no message count, rate, bandwidth
  or latency, so its throughput can only be inferred from CPU. Ours reports all
  four, which is why our side of the table can be checked and theirs cannot.
- **GPU.** Everything here is llvmpipe under Xvfb. Both clients are affected,
  but a real GPU would change the 1080p row most.

## What could not be measured (first pass)

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
