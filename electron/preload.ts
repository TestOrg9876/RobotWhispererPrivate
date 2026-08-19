/**
 * Preload bridge.
 *
 * The renderer needs exactly two things from the shell: where the daemon is
 * listening, and the token to authenticate with. Everything else it does over
 * that socket directly, so no `ipcRenderer` surface is exposed at all — there
 * is nothing here for a compromised renderer to escalate through.
 *
 * The values arrive via `additionalArguments` rather than an IPC round-trip so
 * they are available synchronously, before any renderer code runs.
 */
import { contextBridge } from "electron";

function argValue(prefix: string): string {
  const found = process.argv.find((arg) => arg.startsWith(prefix));
  return found ? found.slice(prefix.length) : "";
}

const port = argValue("--rw-daemon-port=");
const token = argValue("--rw-daemon-token=");

export interface RwNativeBridge {
  rpcUrl: string;
  ingestUrl: string;
}

const bridge: RwNativeBridge = {
  rpcUrl: `ws://127.0.0.1:${port}/rpc?token=${token}`,
  ingestUrl: `ws://127.0.0.1:${port}/ingest?token=${token}`,
};

contextBridge.exposeInMainWorld("__RW_NATIVE__", bridge);
