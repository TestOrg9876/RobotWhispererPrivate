/**
 * Client for the `rw-daemon` JSON-RPC control plane.
 *
 * This replaces Tauri's `invoke`. It is only the *control* path — request /
 * response plus a few server pushes. Bulk ROS frames never come through here;
 * they go straight from the daemon into the decoder Web Worker over a separate
 * binary socket, exactly as they did before this port.
 */

export interface RpcErrorShape {
  kind: string;
  message: string;
}

/** Error carrying the daemon's structured `{ kind, message }` payload. */
export class DaemonRpcError extends Error {
  readonly kind: string;

  constructor(shape: RpcErrorShape) {
    super(shape.message);
    this.name = "DaemonRpcError";
    this.kind = shape.kind;
  }
}

type Pending = {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
};

export type ActionPush = { goalId: string; envelope: unknown };
export type StatusPush = { connectionId: string; status: unknown };
export type DiscoveryPush = { connectionId: string; snapshot: unknown };

interface NativeBridge {
  rpcUrl: string;
  ingestUrl: string;
}

function bridge(): NativeBridge {
  const found = (globalThis as { __RW_NATIVE__?: NativeBridge }).__RW_NATIVE__;
  if (!found) {
    throw new Error(
      "__RW_NATIVE__ is missing: the Electron preload did not run. " +
        "This build must be launched through the Electron shell, not opened directly.",
    );
  }
  return found;
}

class DaemonClient {
  private socket: WebSocket | null = null;
  private ready: Promise<WebSocket> | null = null;
  private nextId = 1;
  private pending = new Map<number, Pending>();

  private actionHandlers = new Map<string, (envelope: unknown) => void>();
  private statusHandlers = new Map<string, (status: unknown) => void>();
  private discoveryHandlers = new Map<string, (snapshot: unknown) => void>();

  ingestUrl(): string {
    return bridge().ingestUrl;
  }

  private connect(): Promise<WebSocket> {
    if (this.ready) return this.ready;
    this.ready = new Promise<WebSocket>((resolve, reject) => {
      const socket = new WebSocket(bridge().rpcUrl);
      socket.onopen = () => {
        this.socket = socket;
        resolve(socket);
      };
      socket.onerror = () => {
        // Only meaningful before open; afterwards `onclose` does the cleanup.
        if (this.socket !== socket) {
          this.ready = null;
          reject(new Error("could not connect to rw-daemon"));
        }
      };
      socket.onclose = () => {
        this.socket = null;
        this.ready = null;
        // Fail every in-flight call rather than leaving callers hanging
        // forever on a socket that is never going to answer.
        const inflight = [...this.pending.values()];
        this.pending.clear();
        for (const entry of inflight) {
          entry.reject(new Error("rw-daemon connection closed"));
        }
      };
      socket.onmessage = (event: MessageEvent) => this.onMessage(event);
    });
    return this.ready;
  }

  private onMessage(event: MessageEvent): void {
    let message: Record<string, unknown>;
    try {
      message = JSON.parse(event.data as string) as Record<string, unknown>;
    } catch (err) {
      console.warn("[daemonClient] malformed message", err);
      return;
    }

    if (typeof message.push === "string") {
      this.onPush(message);
      return;
    }

    const id = message.id as number | undefined;
    if (typeof id !== "number") return;
    const entry = this.pending.get(id);
    if (!entry) return;
    this.pending.delete(id);

    if (message.err) {
      entry.reject(new DaemonRpcError(message.err as RpcErrorShape));
    } else {
      entry.resolve(message.ok);
    }
  }

  private onPush(message: Record<string, unknown>): void {
    switch (message.push) {
      case "action": {
        const handler = this.actionHandlers.get(message.goalId as string);
        if (handler) handler(message.envelope);
        // The daemon always terminates a goal with a `closed` envelope, which
        // is the point where the handler can be dropped.
        const envelope = message.envelope as { kind?: string } | undefined;
        if (envelope?.kind === "closed") this.actionHandlers.delete(message.goalId as string);
        break;
      }
      case "status": {
        this.statusHandlers.get(message.connectionId as string)?.(message.status);
        break;
      }
      case "discovery": {
        this.discoveryHandlers.get(message.connectionId as string)?.(message.snapshot);
        break;
      }
      default:
        break;
    }
  }

  async call<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    const socket = await this.connect();
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
      socket.send(JSON.stringify({ id, method, params }));
    });
  }

  onAction(goalId: string, handler: (envelope: unknown) => void): void {
    this.actionHandlers.set(goalId, handler);
  }

  onStatus(connectionId: string, handler: (status: unknown) => void): void {
    this.statusHandlers.set(connectionId, handler);
  }

  onDiscovery(connectionId: string, handler: (snapshot: unknown) => void): void {
    this.discoveryHandlers.set(connectionId, handler);
  }
}

export const daemonClient = new DaemonClient();
