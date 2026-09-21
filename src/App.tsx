import { useEffect, useState } from "react";
import { useSystemInfo } from "./hooks/useSystemInfo";
import { useAuth } from "./hooks/useAuth";
import { useJobRunner } from "./hooks/useJobRunner";
import { useNodeConnection } from "./hooks/useNodeConnection";
import { useProvisioning } from "./hooks/useProvisioning";
import { loadSettings, saveSettings } from "./lib/settings";
import { StatBar } from "./components/StatBar";
import { LoginForm } from "./components/LoginForm";
import { RuntimeSetup } from "./components/RuntimeSetup";
import type { ResourceLimits } from "./types";
import "./App.css";

function formatBytes(bytes: number): string {
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

function App() {
  const { systemInfo, error } = useSystemInfo();
  const [settings, setSettings] = useState<ResourceLimits>(loadSettings);
  const { session, login, register, logout } = useAuth();
  const {
    nodeId,
    connectionStatus,
    error: connectionError,
  } = useNodeConnection(session, systemInfo, settings, logout);
  const runner = useJobRunner(session, nodeId, systemInfo, settings, logout);
  const provisioning = useProvisioning();

  useEffect(() => {
    saveSettings(settings);
  }, [settings]);

  // Installing the runtime changes what the sandbox probe would say, so ask again.
  const runtimeReady = provisioning.status?.ready === true;
  const { refreshSandbox } = runner;
  useEffect(() => {
    if (runtimeReady) void refreshSandbox();
  }, [runtimeReady, refreshSandbox]);

  if (!session) {
    return <LoginForm onLogin={login} onRegister={register} />;
  }

  const status = !settings.contributingEnabled
    ? "Off"
    : systemInfo?.is_idle
      ? "Idle · ready for jobs"
      : "Busy · not contributing";

  const backendStatusText =
    connectionStatus === "connected"
      ? `Connected to backend as ${session.email}`
      : connectionStatus === "error"
        ? `Backend connection issue: ${connectionError ?? "unknown error"}`
        : "Connecting to backend…";

  return (
    <main className="container">
      <header className="header">
        <div>
          <h1>SpareCycles</h1>
          <p className="status-line">{status}</p>
        </div>
        <div className="header-actions">
          <label className="toggle">
            <input
              type="checkbox"
              checked={settings.contributingEnabled}
              onChange={(e) =>
                setSettings((s) => ({
                  ...s,
                  contributingEnabled: e.target.checked,
                }))
              }
            />
            <span>{settings.contributingEnabled ? "Contributing" : "Paused"}</span>
          </label>
          <button type="button" className="link-button" onClick={logout}>
            Sign out
          </button>
        </div>
      </header>

      <p className={connectionStatus === "error" ? "error" : "backend-status"}>{backendStatusText}</p>

      {error && <p className="error">Couldn't read system stats: {error}</p>}

      <section className="card">
        <h2>Resource usage</h2>
        {systemInfo ? (
          <>
            <StatBar
              label="CPU"
              percent={systemInfo.cpu_usage_percent}
              sublabel={`${systemInfo.cpu_core_count} cores`}
            />
            <StatBar
              label="Memory"
              percent={
                (systemInfo.memory_used_bytes / systemInfo.memory_total_bytes) *
                100
              }
              sublabel={`${formatBytes(systemInfo.memory_used_bytes)} / ${formatBytes(systemInfo.memory_total_bytes)}`}
            />
            {systemInfo.gpu ? (
              systemInfo.gpu.usage_percent !== null ? (
                <StatBar
                  label="GPU"
                  percent={systemInfo.gpu.usage_percent}
                  sublabel={systemInfo.gpu.name}
                />
              ) : (
                <div className="stat-bar stat-bar-unavailable">
                  <span>GPU · {systemInfo.gpu.name}</span>
                  <span>usage unavailable</span>
                </div>
              )
            ) : (
              <div className="stat-bar stat-bar-unavailable">
                <span>GPU</span>
                <span>not detected</span>
              </div>
            )}
          </>
        ) : (
          <p>Reading system stats…</p>
        )}
      </section>

      <section className="card">
        <h2>Job execution</h2>
        {runner.currentJob ? (
          <p className="job-line">
            Rendering frame {runner.currentJob.params.frame} · job{" "}
            {runner.currentJob.job_id.slice(0, 8)}
          </p>
        ) : (
          <p className="job-line">
            {settings.contributingEnabled ? "Waiting for work" : "Paused — not accepting jobs"}
          </p>
        )}

        {provisioning.status && !provisioning.status.ready && (
          <RuntimeSetup
            status={provisioning.status}
            progress={provisioning.progress}
            installing={provisioning.installing}
            error={provisioning.error}
            onInstall={() => void provisioning.install()}
          />
        )}

        {runner.sandbox ? (
          <>
            <p className={runner.sandbox.gvisor_available ? "sandbox-line" : "error"}>
              {!runner.sandbox.engine_available
                ? `The job runtime isn't available (${runner.sandbox.engine}) — this machine can't accept jobs yet`
                : !runner.sandbox.gvisor_available
                  ? "gVisor isn't available here, so jobs are refused. Running a stranger's code without it isn't something to do quietly."
                  : !runner.sandbox.image_present
                    ? "Sandbox ready, but the render image isn't installed yet"
                    : "Sandbox ready · gVisor"}
            </p>
            <p className="sandbox-line">{runner.sandbox.isolation}</p>
          </>
        ) : (
          <p className="sandbox-line">Checking sandbox…</p>
        )}

        {runner.lastOutcome && (
          <p className="sandbox-line">
            Last job: {runner.lastOutcome.success ? "completed" : "failed"} · granted{" "}
            {runner.lastOutcome.effective_caps.cpu_cores} cores,{" "}
            {runner.lastOutcome.effective_caps.memory_mb} MB
          </p>
        )}

        {runner.error && <p className="error">{runner.error}</p>}
      </section>

      <section className="card">
        <h2>Earnings</h2>
        <p className="earnings">$0.00</p>
        <p className="earnings-note">Payouts arrive in a later phase.</p>
      </section>

      <section className="card">
        <h2>Limits</h2>
        <label className="slider-row">
          <span>Max CPU usage: {settings.maxCpuPercent}%</span>
          <input
            type="range"
            min={10}
            max={100}
            value={settings.maxCpuPercent}
            onChange={(e) =>
              setSettings((s) => ({
                ...s,
                maxCpuPercent: Number(e.target.value),
              }))
            }
          />
        </label>
        <label className="slider-row">
          <span>Max GPU usage: {settings.maxGpuPercent}%</span>
          <input
            type="range"
            min={10}
            max={100}
            value={settings.maxGpuPercent}
            onChange={(e) =>
              setSettings((s) => ({
                ...s,
                maxGpuPercent: Number(e.target.value),
              }))
            }
          />
        </label>
        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={settings.neverWhileGaming}
            onChange={(e) =>
              setSettings((s) => ({
                ...s,
                neverWhileGaming: e.target.checked,
              }))
            }
          />
          <span>Never run jobs while gaming</span>
        </label>
      </section>
    </main>
  );
}

export default App;
