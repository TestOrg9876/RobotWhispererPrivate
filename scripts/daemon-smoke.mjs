#!/usr/bin/env node
/**
 * Drives `rw-daemon` over its JSON-RPC socket with no frontend involved.
 *
 * This is the real test of the port's central claim: the Rust core is
 * frontend-agnostic. If this script passes, the core is genuinely driveable by
 * anything that speaks the protocol — Electron today, something else tomorrow.
 *
 * It also verifies the two things most likely to break silently:
 *   - the auth/origin checks actually reject, rather than being decorative;
 *   - packed frames still carry WIRE_VERSION 4, so the untouched TypeScript
 *     decoder can read what the new daemon writes.
 *
 * Usage: node scripts/daemon-smoke.mjs [path-to-rw-daemon]
 */
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const DAEMON = process.argv[2] ?? "core/target/release/rw-daemon";
const WIRE_VERSION = 4;

let passed = 0;
let failed = 0;

function check(name, condition, detail = "") {
  if (condition) {
    passed++;
    console.log(`  ok   ${name}`);
  } else {
    failed++;
    console.error(`  FAIL ${name}${detail ? ` — ${detail}` : ""}`);
  }
}

function startDaemon(dataDir) {
  const child = spawn(DAEMON, [`--data-dir=${dataDir}`], { stdio: ["pipe", "pipe", "pipe"] });
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (line) => {
    if (process.env.RW_SMOKE_VERBOSE) process.stderr.write(`[daemon] ${line}`);
  });
  return new Promise((resolve, reject) => {
    let buffered = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      buffered += chunk;
      const newline = buffered.indexOf("\n");
      if (newline === -1) return;
      resolve({ child, handshake: JSON.parse(buffered.slice(0, newline)) });
    });
    child.on("exit", (code) => reject(new Error(`daemon exited early (${code})`)));
    child.on("error", reject);
  });
}

/** Minimal promise-correlating RPC client, mirroring src/lib/core/daemonClient.ts. */
function connectRpc(url) {
  const socket = new WebSocket(url);
  const pending = new Map();
  const pushes = [];
  let nextId = 1;

  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (message.push) {
      pushes.push(message);
      return;
    }
    const entry = pending.get(message.id);
    if (!entry) return;
    pending.delete(message.id);
    if (message.err) entry.reject(new Error(`${message.err.kind}: ${message.err.message}`));
    else entry.resolve(message.ok);
  });

  const ready = new Promise((resolve, reject) => {
    socket.addEventListener("open", () => resolve());
    socket.addEventListener("error", () => reject(new Error(`could not connect to ${url}`)));
  });

  return {
    ready,
    pushes,
    call(method, params = {}) {
      const id = nextId++;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        socket.send(JSON.stringify({ id, method, params }));
      });
    },
    close: () => socket.close(),
  };
}

function expectRejected(url, label) {
  return new Promise((resolve) => {
    const socket = new WebSocket(url);
    const timer = setTimeout(() => {
      socket.close();
      resolve(false);
    }, 3000);
    socket.addEventListener("open", () => {
      clearTimeout(timer);
      socket.close();
      resolve(false); // opened == not rejected == test failure
    });
    socket.addEventListener("error", () => {
      clearTimeout(timer);
      resolve(true);
    });
  }).then((rejected) => check(label, rejected, "socket was accepted but should not have been"));
}

const dataDir = mkdtempSync(path.join(tmpdir(), "rw-smoke-"));
let child;

try {
  console.log(`\nrw-daemon smoke test (data dir: ${dataDir})\n`);
  const started = await startDaemon(dataDir);
  child = started.child;
  const { port, token } = started.handshake;

  check("daemon announces a port", typeof port === "number" && port > 0, `got ${port}`);
  check("daemon announces a token", typeof token === "string" && token.length === 64);

  console.log("\nauth:");
  await expectRejected(`ws://127.0.0.1:${port}/rpc?token=wrong`, "bad token is rejected");
  await expectRejected(`ws://127.0.0.1:${port}/rpc`, "missing token is rejected");
  await expectRejected(`ws://127.0.0.1:${port}/nope?token=${token}`, "unknown path is rejected");

  console.log("\ncontrol plane:");
  const rpc = connectRpc(`ws://127.0.0.1:${port}/rpc?token=${token}`);
  await rpc.ready;
  check("authenticated client connects", true);

  const connectionId = await rpc.call("pipeline_open_dummy");
  check("pipeline_open_dummy returns a connection id", typeof connectionId === "string");

  const discoveryJson = await rpc.call("pipeline_discovery", { connectionId });
  const discovery = JSON.parse(discoveryJson);
  check(
    "discovery lists dummy topics",
    Array.isArray(discovery?.topics) && discovery.topics.length > 0,
    `topics=${discovery?.topics?.length}`,
  );

  console.log("\nworkspace (SQLite round-trip):");
  const created = await rpc.call("create_collection", {
    draft: { name: "smoke", description: null },
  });
  check("create_collection persists", created?.name === "smoke");
  const collections = await rpc.call("list_collections");
  check(
    "list_collections reads it back",
    collections.some((c) => c.name === "smoke"),
  );
  const exported = await rpc.call("export_workspace_command");
  check("export_workspace_command returns JSON", exported.includes("smoke"));

  console.log("\nschema registry:");
  const summaries = await rpc.call("list_schemas_summary");
  check("bundled schemas were installed", summaries.length > 0, `count=${summaries.length}`);

  console.log("\nhot path (binary ingest):");
  const topic = discovery.topics[0].name;
  const ingest = new WebSocket(`ws://127.0.0.1:${port}/ingest?token=${token}`);
  ingest.binaryType = "arraybuffer";
  const firstFrame = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("no frame within 10s")), 10_000);
    ingest.addEventListener("message", (event) => {
      clearTimeout(timer);
      resolve(event.data);
    });
    ingest.addEventListener("error", reject);
  });
  await new Promise((resolve) => ingest.addEventListener("open", resolve));

  const sub = await rpc.call("pipeline_subscribe_topic", { connectionId, topic });
  check(
    "pipeline_subscribe_topic returns a subscription",
    typeof sub?.subscription_id === "string",
  );

  const frame = new Uint8Array(await firstFrame);
  check("a binary frame arrives on the ingest socket", frame.byteLength > 0);
  check(
    `frame declares WIRE_VERSION ${WIRE_VERSION}`,
    frame[0] === WIRE_VERSION,
    `got version byte ${frame[0]}`,
  );
  // Header layout per decoderCore.ts: [0]=version [1]=kind [2..4)=flags
  // [4..12)=timestamp [12..16)=u32 handle length, then the handle bytes.
  const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
  const handleLength = view.getUint32(12, true);
  const handle = new TextDecoder().decode(frame.subarray(16, 16 + handleLength));
  check(
    "frame handle matches the subscription id",
    handle === sub.subscription_id,
    `handle=${handle} sub=${sub.subscription_id}`,
  );

  console.log("\npush channel:");
  await rpc.call("pipeline_watch", { connectionId });
  check("pipeline_watch is accepted", true);

  await rpc.call("pipeline_unsubscribe", { subscriptionId: sub.subscription_id });
  await rpc.call("pipeline_close", { connectionId });
  check("unsubscribe and close succeed", true);

  ingest.close();
  rpc.close();
} catch (err) {
  failed++;
  console.error(`\nFATAL: ${err.message}`);
} finally {
  if (child) {
    child.stdin.end();
    child.kill("SIGTERM");
  }
  rmSync(dataDir, { recursive: true, force: true });
}

console.log(`\n${passed} passed, ${failed} failed\n`);
process.exit(failed === 0 ? 0 : 1);
