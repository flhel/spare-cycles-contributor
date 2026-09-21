import type { AuthSession } from "../types";

const SESSION_STORAGE_KEY = "sparecycles.session";
const NODE_ID_STORAGE_KEY = "sparecycles.nodeId";

export function loadSession(): AuthSession | null {
  try {
    const raw = localStorage.getItem(SESSION_STORAGE_KEY);
    return raw ? (JSON.parse(raw) as AuthSession) : null;
  } catch {
    return null;
  }
}

export function saveSession(session: AuthSession): void {
  localStorage.setItem(SESSION_STORAGE_KEY, JSON.stringify(session));
}

export function clearSession(): void {
  localStorage.removeItem(SESSION_STORAGE_KEY);
  localStorage.removeItem(NODE_ID_STORAGE_KEY);
}

export function loadNodeId(): string | null {
  return localStorage.getItem(NODE_ID_STORAGE_KEY);
}

export function saveNodeId(nodeId: string): void {
  localStorage.setItem(NODE_ID_STORAGE_KEY, nodeId);
}
