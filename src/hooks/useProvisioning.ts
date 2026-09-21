import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ProvisionOutcome, ProvisionProgress, ProvisionStatus } from "../types";

/**
 * Installing the job runtime.
 *
 * The install takes minutes and moves most of a gigabyte, so the Rust side
 * reports progress as events rather than returning once at the end.
 */
export function useProvisioning() {
  const [status, setStatus] = useState<ProvisionStatus | null>(null);
  const [progress, setProgress] = useState<ProvisionProgress | null>(null);
  const [outcome, setOutcome] = useState<ProvisionOutcome | null>(null);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await invoke<ProvisionStatus>("check_provision_status"));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const unlisten = listen<ProvisionProgress>("provision:progress", (event) =>
      setProgress(event.payload),
    );
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, []);

  const install = useCallback(async () => {
    setInstalling(true);
    setError(null);
    setProgress(null);
    try {
      setOutcome(await invoke<ProvisionOutcome>("install_job_runtime"));
    } catch (err: unknown) {
      // The Rust side returns a string explaining what to do about it.
      setError(typeof err === "string" ? err : err instanceof Error ? err.message : String(err));
    } finally {
      setInstalling(false);
      await refresh();
    }
  }, [refresh]);

  return { status, progress, outcome, installing, error, install, refresh };
}
