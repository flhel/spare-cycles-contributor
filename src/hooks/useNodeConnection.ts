import { useEffect, useRef, useState } from "react";
import { ApiError, registerNode, sendHeartbeat } from "../lib/api";
import { loadNodeId, saveNodeId } from "../lib/session";
import type { AuthSession, NodeStatus, ResourceLimits, SystemInfo } from "../types";

const HEARTBEAT_INTERVAL_MS = 15000;

type ConnectionStatus = "connecting" | "connected" | "error";

export function useNodeConnection(
  session: AuthSession | null,
  systemInfo: SystemInfo | null,
  settings: ResourceLimits,
  onUnauthorized: () => void,
) {
  const [nodeId, setNodeId] = useState<string | null>(loadNodeId);
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>("connecting");
  const [error, setError] = useState<string | null>(null);
  const registering = useRef(false);

  // Register this device as a contributor node once, the first time we have
  // both a session and enough system info to describe it.
  useEffect(() => {
    if (!session || !systemInfo || nodeId || registering.current) return;

    registering.current = true;
    registerNode(session.token, {
      name: systemInfo.host_name ?? "Unnamed contributor",
      cpu_core_count: systemInfo.cpu_core_count,
      gpu_name: systemInfo.gpu?.name ?? null,
      max_cpu_percent: settings.maxCpuPercent,
      max_gpu_percent: settings.maxGpuPercent,
    })
      .then((node) => {
        saveNodeId(node.id);
        setNodeId(node.id);
        setConnectionStatus("connected");
        setError(null);
      })
      .catch((err: unknown) => {
        if (err instanceof ApiError && err.status === 401) {
          onUnauthorized();
          return;
        }
        setConnectionStatus("error");
        setError(err instanceof Error ? err.message : "failed to register node");
      })
      .finally(() => {
        registering.current = false;
      });
  }, [session, systemInfo, nodeId, settings.maxCpuPercent, settings.maxGpuPercent, onUnauthorized]);

  // Once registered, report status on an interval so the backend can tell
  // this node is alive (Phase 2 goal: register + heartbeat).
  useEffect(() => {
    if (!session || !nodeId) return;

    const token = session.token;
    const id = nodeId;
    const nodeStatus: NodeStatus = !settings.contributingEnabled
      ? "offline"
      : systemInfo?.is_idle
        ? "online"
        : "busy";

    let cancelled = false;

    async function beat() {
      try {
        await sendHeartbeat(token, id, nodeStatus);
        if (cancelled) return;
        setConnectionStatus("connected");
        setError(null);
      } catch (err) {
        if (cancelled) return;
        if (err instanceof ApiError && err.status === 401) {
          onUnauthorized();
          return;
        }
        if (err instanceof ApiError && err.status === 404) {
          // The node no longer exists server-side; drop the stale id and
          // let the registration effect create a fresh one.
          setNodeId(null);
          return;
        }
        setConnectionStatus("error");
        setError(err instanceof Error ? err.message : "heartbeat failed");
      }
    }

    beat();
    const interval = setInterval(beat, HEARTBEAT_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [session, nodeId, settings.contributingEnabled, systemInfo?.is_idle, onUnauthorized]);

  return { nodeId, connectionStatus, error };
}
