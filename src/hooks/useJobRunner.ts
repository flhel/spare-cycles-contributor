import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ApiError, BACKEND_URL, claimJob } from "../lib/api";
import type {
  AuthSession,
  ClaimedJob,
  JobOutcome,
  ResourceLimits,
  SandboxStatus,
  SystemInfo,
} from "../types";

const POLL_INTERVAL_MS = 10000;

/**
 * Ceilings the contributor's settings imply but the UI doesn't ask for yet.
 * Memory is a conservative fixed cap until there's a control for it.
 */
const MAX_MEMORY_MB = 4096;
const MAX_TIMEOUT_SECONDS = 3600;

export interface RunnerState {
  sandbox: SandboxStatus | null;
  currentJob: ClaimedJob | null;
  lastOutcome: JobOutcome | null;
  error: string | null;
}

/**
 * Polls the backend for work whenever this node is contributing and idle, then
 * runs the job in the sandbox. One job at a time — no queueing on the node.
 */
export function useJobRunner(
  session: AuthSession | null,
  nodeId: string | null,
  systemInfo: SystemInfo | null,
  settings: ResourceLimits,
  onUnauthorized: () => void,
) {
  const [sandbox, setSandbox] = useState<SandboxStatus | null>(null);
  const [currentJob, setCurrentJob] = useState<ClaimedJob | null>(null);
  const [lastOutcome, setLastOutcome] = useState<JobOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const busy = useRef(false);

  /** Re-probes the sandbox — after installing the runtime, there's a new answer. */
  const refreshSandbox = useCallback(async () => {
    try {
      setSandbox(await invoke<SandboxStatus>("check_sandbox"));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void refreshSandbox();
  }, [refreshSandbox]);

  const canRun =
    session !== null &&
    nodeId !== null &&
    settings.contributingEnabled &&
    systemInfo?.is_idle === true &&
    sandbox?.engine_available === true &&
    sandbox?.image_present === true;

  const tick = useCallback(async () => {
    if (!session || !nodeId || busy.current) return;

    busy.current = true;
    try {
      const job = await claimJob(session.token, nodeId);
      if (!job) return;

      setCurrentJob(job);
      setError(null);
      const outcome = await invoke<JobOutcome>("run_job", {
        request: {
          assignment_id: job.assignment_id,
          job_id: job.job_id,
          params: job.params,
          limits: {
            max_cpu_percent: settings.maxCpuPercent,
            max_memory_mb: MAX_MEMORY_MB,
            max_timeout_seconds: MAX_TIMEOUT_SECONDS,
          },
          backend_url: BACKEND_URL,
          token: session.token,
        },
      });
      setLastOutcome(outcome);
      if (!outcome.success) {
        setError(outcome.timed_out ? "job exceeded its time cap" : "render failed — see logs");
      }
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) {
        onUnauthorized();
        return;
      }
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCurrentJob(null);
      busy.current = false;
    }
  }, [session, nodeId, settings.maxCpuPercent, onUnauthorized]);

  useEffect(() => {
    if (!canRun) return;

    tick();
    const interval = setInterval(tick, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [canRun, tick]);

  return { sandbox, currentJob, lastOutcome, error, refreshSandbox };
}
