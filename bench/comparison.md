# Tauri 2 → Electron 43: measured comparison

All figures are **release builds** of the same commit's application code, measured on one
machine (4 vCPU, 15 GiB RAM, Ubuntu 24.04 container, x86_64). Raw data:
[`baseline-tauri.json`](./baseline-tauri.json), [`electron.json`](./electron.json).

Read the caveats at the bottom before quoting any number. Two of them matter a lot.

## The headline

|                          | Tauri 2     | Electron 43 |                                    |
| ------------------------ | ----------- | ----------- | ---------------------------------- |
| **Runs on Ubuntu 20.04** | **No**      | **Yes**     | the reason for the port            |
| deb                      | 11.9 MB     | 92.2 MB     | not comparable, see below          |
| **AppImage**             | **82.4 MB** | **92.5 MB** | **the fair size comparison: +12%** |
| Installed on disk        | 27.5 MB     | 284 MB      |                                    |
| Memory at idle (PSS)     | 106 MB      | 278 MB      | +162%                              |
| Time to visible window   | 109 ms      | 213 ms      | both fast; see caveat 2            |
| Rust binary              | 27.4 MB     | 6.9 MB      | **−75%**                           |
| Rust release build       | 253 s       | 132 s       | **−48%**                           |
| Cargo packages           | 556         | 180         | **−68%**                           |
| Frontend payload         | 12 MB       | 12 MB       | unchanged                          |
| Frontend build           | 37 s        | 37 s        | unchanged                          |

## Package size: the deb comparison is a trap

The 11.9 MB vs 92.2 MB deb gap is real but misleading, and it is worth being precise about
because it is the crux of the whole problem.

Tauri's deb declares:

```
Depends: libwebkit2gtk-4.1-0, libgtk-3-0
```

It is small because **it does not contain a browser engine** — it borrows the system's. That
single line is also the Ubuntu 20.04 failure, exactly: 20.04 ships `libwebkit2gtk-4.0-37`,
and `4.1` only appears in 22.04. The package is not merely buggy on 20.04, it is
_uninstallable_ there — apt cannot satisfy the dependency at all.

The moment you ask Tauri for a self-contained artifact, it has to bundle WebKitGTK and GTK
itself, and the advantage collapses:

- Tauri AppImage: **82.4 MB**
- Electron AppImage: **92.5 MB**

**That is the apples-to-apples number: 12% larger, not 8× larger.** This is the concrete
version of the complaint that Tauri "doesn't provide smaller package sizes" for snap and
flatpak — those formats are self-contained by construction, so they land in the same place
the AppImage does.

Where the Electron build genuinely costs more is **installed footprint**: 284 MB vs 27.5 MB,
because Chromium is unpacked on disk rather than shared with the system.

### What the size work bought

Applied and measured, against a default electron-builder configuration:

| Change                                             | Saved                              |
| -------------------------------------------------- | ---------------------------------- |
| `electronLanguages: [en-US]` (55 locale packs → 1) | 46 MB unpacked                     |
| Excluding `node_modules` from the asar             | 27 MB unpacked (asar 27 MB → 9 KB) |
| `compression: maximum` + asar                      | —                                  |
| Rust: fat LTO, 1 codegen unit, `strip`             | binary 27.4 → 6.9 MB               |

`node_modules` was pure waste: electron-builder ships `dependencies` into the asar by default,
but Vite had already inlined three.js, uplot and urdf-loader into the renderer bundle, so they
were being shipped twice and loaded never.

SwiftShader (4.5 MB) and `libvulkan` (2.4 MB) were **kept** deliberately. Dropping them saves
~7 MB but removes the software-GL fallback, and a robotics workstation or VM with no working
GPU driver would render the 3D viewer black instead of slowly.

## Your robot models were never the problem

`static/` is 114 MB of `.dae`/`.obj` source meshes, but `vite/mesh-optimize.ts` transcodes them
to Draco-compressed GLB at `closeBundle` and deletes the sources from the output. Measured:

```
static/  114 MB  (50 .dae + 26 .obj, sources)
build/    12 MB  (76 .glb + 3.6 MB Draco decoder)
```

So the shipped frontend is 12 MB, identical before and after the port — the mesh pipeline is
untouched. Package size is dominated by the Chromium runtime, not by your robots.

## Memory and startup

|                         | Tauri        | Electron     |
| ----------------------- | ------------ | ------------ |
| Processes               | 5            | 11           |
| RSS (summed)            | 184.8 MB     | 585.0 MB     |
| **PSS (honest figure)** | **106.1 MB** | **277.6 MB** |
| Time to mapped window   | 109 ms       | 213 ms       |

PSS is the number to use: summed RSS counts shared Chromium pages once per process and
overstates Electron badly. Even so, Electron costs roughly 2.6× the memory at idle. That is the
real price of a bundled engine, and no configuration removes it.

## What got faster

**The Rust side, substantially.** Removing `tauri`, `wry`, `webkit2gtk`, `gtk`, `soup3` and the
`objc2`/`windows` families took the dependency graph from 556 packages to 180. Release build
253 s → 132 s, binary 27.4 MB → 6.9 MB. CI also no longer installs
`libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev`.

**Startup does one less round-trip.** Under Tauri the renderer had to `invoke("ingest_ws_port")`
before it could open the frame socket. The Electron shell passes the port and token to the
renderer at load, so the first subscription streams one round-trip sooner.

**Discovery and status are pushed, not polled.** `pipelineRpc.tauri.ts` had `onDiscovery` and
`onStatus` as empty stubs, so the native build polled `getDiscovery()` on a 750 ms timer while
the WASM build had real push. The daemon now implements `pipeline_watch`, so both do.

**One memcpy per frame removed on the hot path.** The old ingest hub was a
`broadcast::Sender<Arc<Vec<u8>>>`, and every client had to clone the frame into an owned `Vec`
for tungstenite. There is exactly one consumer (the decoder worker), so it is now a
single-consumer `mpsc` that moves the `Vec` straight into the message with no copy.

**The frame path itself is unchanged and that is deliberate.** `decoder.worker.ts`,
`decoderCore.ts`, `cborDecode.ts` and `pipelineHub.ts` were not touched. `WIRE_VERSION` is still
4 and the perf-trace tail still carries every field. The hot path already bypassed Tauri IPC via
a loopback WebSocket, so the correct move was to keep it, not rewrite it.

## Caveats — read these

1. **GPU rendering was not measured, and it is probably the biggest real-world difference.**
   This container is headless with software GL (`libEGL warning: DRI3 error` in both runs), so
   any WebGL or three.js frame-rate number here would describe llvmpipe, not WebKitGTK vs
   Chromium. The expected win for the 3D viewer and streaming plots is the main
   performance argument for Chromium and it is **unverified**. It needs your hardware.

2. **The startup comparison is biased in Tauri's favour.** The Electron shell uses
   `show: false` + `ready-to-show`, so its window appears only when the renderer can paint.
   Tauri maps its GTK window immediately and fills it later. 109 ms vs 213 ms is therefore
   "empty window" vs "window with content" — not the same event.

3. **Snap and flatpak were not built here.** electron-builder shells out to the real `snapcraft`
   (needs snapd) and `flatpak-builder` (needs flathub, which this environment's egress policy
   blocks). Both targets are configured and built by
   [`.github/workflows/release.yml`](../.github/workflows/release.yml).

4. **The 20.04/22.04/24.04 runtime test did not run here either** — Docker Hub's blob CDN is
   also blocked. Compatibility was instead verified statically, which is strong but not the same
   as launching: every shipped ELF requires at most **GLIBC_2.28** and nothing needs `GLIBCXX`
   (libstdc++ is statically linked). Ubuntu 20.04 ships glibc 2.31. The release workflow runs the
   actual install-and-launch matrix on all three releases.

5. **One bug this caught.** A plain `cargo build --release` on Ubuntu 24.04 produced a daemon
   requiring `GLIBC_2.34` — above 20.04's 2.31, which would have silently broken the exact thing
   the port exists to fix, while Electron itself was fine. The daemon is now built with
   `cargo-zigbuild --target x86_64-unknown-linux-gnu.2.28`, and CI asserts the floor so it cannot
   regress.

## Verdict

Electron costs ~170 MB of installed footprint and ~170 MB of RSS. In exchange the app becomes
installable on Ubuntu 20.04 at all, behaves identically across distros, and the self-contained
package — the one that matters for snap, flatpak and AppImage — is only 12% larger than Tauri's.
The Rust core got smaller, faster to build, and genuinely frontend-agnostic.

For a desktop robotics tool, that is a good trade. If installed size were the dominant
constraint it would not be.
