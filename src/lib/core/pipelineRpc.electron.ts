import { decoderWorker } from "$lib/workers/decoderManager";
import { daemonClient } from "./daemonClient";
import type { Value } from "./types";
import type {
  ActionEnvelope,
  ConnectionStatus,
  DiscoverySnapshot,
  FrameCallback,
  PipelineRpc,
  SubscribeOptions,
  SubscribeResponse,
} from "./pipelineRpc.shared";

/**
 * Options travel to the daemon with snake_case inner keys. This is the same
 * shape the Tauri build sent, kept so the Rust side did not have to change.
 */
function optionsToBackend(options: SubscribeOptions | undefined): unknown {
  if (!options) return null;
  const payload: Record<string, unknown> = {};
  if (typeof options.targetHz === "number") payload.target_hz = options.targetHz;
  if (typeof options.queueLength === "number") payload.queue_length = options.queueLength;
  if (Array.isArray(options.fields) && options.fields.length > 0) payload.fields = options.fields;
  return Object.keys(payload).length === 0 ? null : payload;
}

class ElectronPipelineRpc implements PipelineRpc {
  private ingestConnected = false;
  /** Connections we have already asked the daemon to push updates for. */
  private watched = new Set<string>();

  /**
   * Under Tauri this required an `invoke("ingest_ws_port")` round-trip before
   * the first subscription could stream. The shell now hands us the URL at
   * startup, so the first frame arrives one round-trip sooner.
   */
  private ensureIngestConnected(): void {
    if (this.ingestConnected) return;
    decoderWorker.connectIngest(daemonClient.ingestUrl());
    this.ingestConnected = true;
  }

  private async ensureWatched(connectionId: string): Promise<void> {
    if (this.watched.has(connectionId)) return;
    this.watched.add(connectionId);
    try {
      await daemonClient.call("pipeline_watch", { connectionId });
    } catch (err) {
      this.watched.delete(connectionId);
      throw err;
    }
  }

  openFoxglove(url: string): Promise<string> {
    return daemonClient.call<string>("pipeline_open_foxglove", { url });
  }

  openRosbridge(url: string): Promise<string> {
    return daemonClient.call<string>("pipeline_open_rosbridge", { url });
  }

  openDummy(): Promise<string> {
    return daemonClient.call<string>("pipeline_open_dummy");
  }

  async close(connectionId: string): Promise<void> {
    this.watched.delete(connectionId);
    await daemonClient.call("pipeline_close", { connectionId });
  }

  async subscribe(
    streamKey: string,
    connectionId: string,
    topic: string,
    onFrame: FrameCallback,
    options?: SubscribeOptions,
  ): Promise<SubscribeResponse> {
    this.ensureIngestConnected();
    decoderWorker.registerStream(streamKey, onFrame);
    const resp = await daemonClient.call<SubscribeResponse>("pipeline_subscribe_topic", {
      connectionId,
      topic,
      options: optionsToBackend(options),
    });
    decoderWorker.mapStream(streamKey, {
      handle: resp.subscription_id,
      schemaId: resp.schema_id,
      schemaName: resp.schema_name,
      vizRole: resp.viz_role,
    });
    return resp;
  }

  async unsubscribe(streamKey: string, subscriptionId: string): Promise<void> {
    decoderWorker.unregisterStream(streamKey);
    decoderWorker.unmapStream(streamKey, subscriptionId);
    try {
      await daemonClient.call("pipeline_unsubscribe", { subscriptionId });
    } catch {
      // The subscription may already be gone if the connection dropped; the
      // local unmapping above is what actually matters to the UI.
    }
  }

  async callService(connectionId: string, service: string, request: Value): Promise<Value> {
    const responseJson = await daemonClient.call<string>("pipeline_call_service", {
      connectionId,
      service,
      requestJson: JSON.stringify(request),
    });
    return JSON.parse(responseJson) as Value;
  }

  async sendActionGoal(
    connectionId: string,
    action: string,
    goal: Value,
    onEnvelope: (envelope: ActionEnvelope) => void,
  ): Promise<string> {
    const goalId = await daemonClient.call<string>("pipeline_send_action_goal", {
      connectionId,
      action,
      goalJson: JSON.stringify(goal),
    });
    daemonClient.onAction(goalId, (envelope) => onEnvelope(envelope as ActionEnvelope));
    return goalId;
  }

  async cancelActionGoal(goalId: string): Promise<void> {
    await daemonClient.call("pipeline_cancel_action_goal", { goalId });
  }

  async getDiscovery(sessionId: string): Promise<DiscoverySnapshot | null> {
    const json = await daemonClient.call<string>("pipeline_discovery", {
      connectionId: sessionId,
    });
    if (!json || json === "null") return null;
    return JSON.parse(json) as DiscoverySnapshot;
  }

  async onDiscovery(sessionId: string, cb: (snapshot: DiscoverySnapshot) => void): Promise<void> {
    daemonClient.onDiscovery(sessionId, (snapshot) => cb(snapshot as DiscoverySnapshot));
    await this.ensureWatched(sessionId);
  }

  async onStatus(sessionId: string, cb: (status: ConnectionStatus) => void): Promise<void> {
    daemonClient.onStatus(sessionId, (status) => cb(status as ConnectionStatus));
    await this.ensureWatched(sessionId);
  }
}

export function create(): PipelineRpc {
  return new ElectronPipelineRpc();
}
