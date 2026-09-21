import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SystemInfo } from "../types";

const POLL_INTERVAL_MS = 2000;

export function useSystemInfo() {
  const [systemInfo, setSystemInfo] = useState<SystemInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function poll() {
      try {
        const info = await invoke<SystemInfo>("get_system_info");
        if (!cancelled) {
          setSystemInfo(info);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) setError(String(err));
      }
    }

    poll();
    const interval = setInterval(poll, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  return { systemInfo, error };
}
