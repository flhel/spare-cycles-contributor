import type { AuthSession, ClaimedJob, ContributorNode, NodeStatus } from "../types";

export const BACKEND_URL: string = import.meta.env.VITE_BACKEND_URL ?? "http://localhost:3000";

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(path: string, options: RequestInit = {}): Promise<T> {
  const res = await fetch(`${BACKEND_URL}${path}`, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      ...options.headers,
    },
  });

  const body = await res.json().catch(() => null);
  if (!res.ok) {
    throw new ApiError(res.status, body?.error ?? `request failed with status ${res.status}`);
  }
  return body as T;
}

interface AuthResponse {
  token: string;
  user: { id: string; email: string };
}

function toSession(response: AuthResponse): AuthSession {
  return { token: response.token, userId: response.user.id, email: response.user.email };
}

export async function login(email: string, password: string): Promise<AuthSession> {
  const res = await request<AuthResponse>("/auth/login", {
    method: "POST",
    body: JSON.stringify({ email, password }),
  });
  return toSession(res);
}

export async function register(email: string, password: string): Promise<AuthSession> {
  const res = await request<AuthResponse>("/auth/register", {
    method: "POST",
    body: JSON.stringify({ email, password }),
  });
  return toSession(res);
}

export interface RegisterNodeInput {
  name: string;
  cpu_core_count: number;
  gpu_name: string | null;
  max_cpu_percent: number;
  max_gpu_percent: number;
}

export async function registerNode(token: string, input: RegisterNodeInput): Promise<ContributorNode> {
  return request<ContributorNode>("/nodes", {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
    body: JSON.stringify(input),
  });
}

/**
 * Asks the backend for work. Returns null when there's nothing to do — the
 * backend answers 204 in that case.
 */
export async function claimJob(token: string, nodeId: string): Promise<ClaimedJob | null> {
  const res = await fetch(`${BACKEND_URL}/nodes/${nodeId}/claim`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
  });

  if (res.status === 204) return null;
  const body = await res.json().catch(() => null);
  if (!res.ok) {
    throw new ApiError(res.status, body?.error ?? `claim failed with status ${res.status}`);
  }
  return body as ClaimedJob;
}

export async function sendHeartbeat(
  token: string,
  nodeId: string,
  status: NodeStatus,
): Promise<ContributorNode> {
  return request<ContributorNode>(`/nodes/${nodeId}/heartbeat`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
    body: JSON.stringify({ status }),
  });
}
