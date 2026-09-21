export interface GpuInfo {
  name: string;
  vendor: string;
  /** null when the GPU is detected but its utilization can't be read here. */
  usage_percent: number | null;
}

export interface SystemInfo {
  host_name: string | null;
  cpu_usage_percent: number;
  cpu_core_count: number;
  memory_used_bytes: number;
  memory_total_bytes: number;
  gpu: GpuInfo | null;
  is_idle: boolean;
}

export interface ResourceLimits {
  maxCpuPercent: number;
  maxGpuPercent: number;
  neverWhileGaming: boolean;
  contributingEnabled: boolean;
}

export interface AuthSession {
  token: string;
  userId: string;
  email: string;
}

export type NodeStatus = "online" | "busy" | "offline";

export interface SandboxStatus {
  engine_available: boolean;
  engine_version: string | null;
  /** Where jobs run, in words — e.g. "Podman in the 'SpareCycles' WSL2 distro". */
  engine: string;
  /** Without gVisor the app refuses jobs rather than running them unsandboxed. */
  gvisor_available: boolean;
  image_present: boolean;
  image_digest: string | null;
  isolation: string;
}

export interface RequestedCaps {
  cpu_cores: number;
  memory_mb: number;
  timeout_seconds: number;
}

export interface RenderJobParams {
  blender_version: string;
  frame: number;
  resolution_x: number;
  resolution_y: number;
  samples: number;
  requested_caps: RequestedCaps;
}

export interface ClaimedJob {
  assignment_id: string;
  job_id: string;
  params: RenderJobParams;
}

export interface EffectiveCaps {
  cpu_cores: number;
  memory_mb: number;
  timeout_seconds: number;
}

export interface JobOutcome {
  success: boolean;
  output_path: string | null;
  exit_code: number | null;
  timed_out: boolean;
  log_tail: string;
  effective_caps: EffectiveCaps;
  used_gvisor: boolean;
}

export interface ContributorNode {
  id: string;
  user_id: string;
  name: string;
  status: NodeStatus;
  cpu_core_count: number | null;
  gpu_name: string | null;
  max_cpu_percent: number;
  max_gpu_percent: number;
  last_heartbeat_at: string | null;
  created_at: string;
}

/** What provisioning found on this machine, and what it would do next. */
export interface ProvisionStatus {
  wsl_available: boolean;
  distro_name: string;
  distro_installed: boolean;
  artifacts_dir: string | null;
  artifacts_present: boolean;
  /** Whether this build knows what the artifacts should hash to. */
  artifacts_pinned: boolean;
  ready: boolean;
  summary: string;
}

/** Step ids: verify, import, load, check, done. */
export interface ProvisionProgress {
  step: string;
  detail: string;
  percent: number;
}

export interface ProvisionOutcome {
  distro_name: string;
  /** False when the build had no pinned digests to check the artifacts against. */
  artifacts_verified: boolean;
  gvisor_available: boolean;
  image_present: boolean;
}
