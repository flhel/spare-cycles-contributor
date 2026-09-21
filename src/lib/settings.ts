import type { ResourceLimits } from "../types";

const STORAGE_KEY = "sparecycles.settings";

const DEFAULT_LIMITS: ResourceLimits = {
  maxCpuPercent: 70,
  maxGpuPercent: 70,
  neverWhileGaming: true,
  contributingEnabled: false,
};

export function loadSettings(): ResourceLimits {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_LIMITS;
    return { ...DEFAULT_LIMITS, ...JSON.parse(raw) };
  } catch {
    return DEFAULT_LIMITS;
  }
}

export function saveSettings(settings: ResourceLimits): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
}
