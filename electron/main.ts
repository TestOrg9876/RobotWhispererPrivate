/**
 * Electron main process.
 *
 * Deliberately thin. It owns the window and the daemon's lifetime and nothing
 * else — in particular it is *not* in the data path. The renderer talks to
 * `rw-daemon` directly over loopback, so ROS frames never cross Electron's IPC
 * boundary. That is the whole reason this port is a sidecar rather than a
 * native addon.
 */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync } from "node:fs";
import { stat } from "node:fs/promises";
import path from "node:path";

import { app, BrowserWindow, protocol, shell, net } from "electron";

const isDev = !app.isPackaged;

/** Where the built SvelteKit SPA lives. */
const rendererDir = isDev
  ? path.join(__dirname, "..", "build")
  : path.join(process.resourcesPath, "build");

/**
 * The daemon ships next to the app resources, outside the asar — a native
 * binary cannot be executed from inside an archive.
 */
function daemonPath(): string {
  if (!isDev) return path.join(process.resourcesPath, "bin", "rw-daemon");
  const root = path.join(__dirname, "..", "core", "target");
  // Prefer the portable (glibc 2.28) build that packaging uses, so a dev run
  // exercises the same binary that ships; fall back to a plain cargo build.
  const candidates = [
    path.join(root, "x86_64-unknown-linux-gnu", "release", "rw-daemon"),
    path.join(root, "release", "rw-daemon"),
    path.join(root, "debug", "rw-daemon"),
  ];
  return candidates.find(existsSync) ?? candidates[0];
}

interface DaemonHandshake {
  port: number;
  token: string;
}

let daemon: ChildProcessWithoutNullStreams | null = null;

/**
 * Start the daemon and wait for it to announce its port and auth token on
 * stdout. Resolving only after that line arrives is what guarantees the
 * listener is accepting before the renderer tries to connect.
 */
function startDaemon(): Promise<DaemonHandshake> {
  const bin = daemonPath();
  if (!existsSync(bin)) {
    return Promise.reject(
      new Error(`rw-daemon not found at ${bin}. Run \`bun run build:daemon\` first.`),
    );
  }

  const child = spawn(bin, [`--data-dir=${app.getPath("userData")}`], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  daemon = child;

  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk: string) => process.stderr.write(`[rw-daemon] ${chunk}`));

  return new Promise<DaemonHandshake>((resolve, reject) => {
    let buffered = "";
    const onData = (chunk: string) => {
      buffered += chunk;
      const newline = buffered.indexOf("\n");
      if (newline === -1) return;
      child.stdout.off("data", onData);
      try {
        resolve(JSON.parse(buffered.slice(0, newline)) as DaemonHandshake);
      } catch (err) {
        reject(new Error(`could not parse daemon handshake: ${String(err)}`));
      }
    };
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", onData);
    child.on("exit", (code) => reject(new Error(`rw-daemon exited early with code ${code}`)));
    child.on("error", reject);
  });
}

function stopDaemon(): void {
  if (!daemon) return;
  // Closing stdin is the daemon's own shutdown signal; SIGTERM is the fallback.
  try {
    daemon.stdin.end();
  } catch {
    /* already gone */
  }
  daemon.kill("SIGTERM");
  daemon = null;
}

/**
 * The renderer is served from `app://` rather than `file://`. Under `file://`
 * Chromium treats every document as an opaque origin, which breaks module Web
 * Workers (the decoder), absolute asset paths (`/assets/...`, the robot
 * catalog) and the WASM MIME check the Draco decoder relies on. A custom
 * standard scheme keeps all three working exactly as they do on the web build.
 */
protocol.registerSchemesAsPrivileged([
  {
    scheme: "app",
    privileges: {
      standard: true,
      secure: true,
      supportFetchAPI: true,
      corsEnabled: true,
      stream: true,
    },
  },
]);

function registerAppProtocol(): void {
  protocol.handle("app", async (request) => {
    const url = new URL(request.url);
    // Resolve inside the renderer directory and verify we stayed there, so a
    // crafted `app://` URL cannot read arbitrary files off disk.
    const requested = path.normalize(decodeURIComponent(url.pathname));
    let target = path.join(rendererDir, requested);
    if (!target.startsWith(rendererDir)) {
      return new Response("forbidden", { status: 403 });
    }

    const isFile = await stat(target)
      .then((s) => s.isFile())
      .catch(() => false);
    if (!isFile) {
      // SPA fallback, matching adapter-static's `fallback: "index.html"`.
      target = path.join(rendererDir, "index.html");
    }

    return net.fetch(`file://${target}`);
  });
}

async function createWindow(handshake: DaemonHandshake): Promise<void> {
  const window = new BrowserWindow({
    width: 1280,
    height: 800,
    show: false,
    backgroundColor: "#111111",
    webPreferences: {
      // `.cjs` because the package is `type: module` but a sandboxed preload
      // must be CommonJS.
      preload: path.join(__dirname, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      // A telemetry dashboard must keep decoding and drawing when it is not the
      // focused window. Chromium throttles background rAF and timers by
      // default, which would silently stall live plots behind another window.
      backgroundThrottling: false,
      additionalArguments: [
        `--rw-daemon-port=${handshake.port}`,
        `--rw-daemon-token=${handshake.token}`,
      ],
    },
  });

  window.once("ready-to-show", () => window.show());

  // External links open in the user's browser, never inside the app shell.
  window.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith("https://") || url.startsWith("http://")) void shell.openExternal(url);
    return { action: "deny" };
  });

  await window.loadURL("app://bundle/index.html");
}

// A second instance would race for the same SQLite workspace file.
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on("second-instance", () => {
    const [existing] = BrowserWindow.getAllWindows();
    if (existing) {
      if (existing.isMinimized()) existing.restore();
      existing.focus();
    }
  });

  app.whenReady().then(async () => {
    registerAppProtocol();
    try {
      const handshake = await startDaemon();
      await createWindow(handshake);
    } catch (err) {
      console.error("[main] startup failed:", err);
      app.quit();
    }

    app.on("activate", () => {
      if (BrowserWindow.getAllWindows().length === 0) {
        console.warn("[main] activate with no windows; daemon handshake is gone, restarting");
        void startDaemon().then(createWindow);
      }
    });
  });

  app.on("window-all-closed", () => {
    if (process.platform !== "darwin") app.quit();
  });

  app.on("before-quit", stopDaemon);
  app.on("will-quit", stopDaemon);
}
