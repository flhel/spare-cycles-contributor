import type { ProvisionProgress, ProvisionStatus } from "../types";

interface Props {
  status: ProvisionStatus;
  progress: ProvisionProgress | null;
  installing: boolean;
  error: string | null;
  onInstall: () => void;
}

/**
 * Shown only while this machine can't run jobs yet.
 *
 * Deliberately explains what is about to happen before offering the button:
 * installing the runtime creates a Linux VM on the contributor's machine, which
 * is not a thing to do behind a spinner.
 */
export function RuntimeSetup({ status, progress, installing, error, onInstall }: Props) {
  const canInstall = status.wsl_available && status.artifacts_present && !installing;

  return (
    <section className="runtime-setup">
      <h2>Set up the job runtime</h2>
      <p className="sandbox-line">{status.summary}</p>

      <p className="sandbox-line">
        Jobs never run directly on your machine. They run inside a small Linux
        environment that SpareCycles installs as a WSL2 distro named{" "}
        <code>{status.distro_name}</code> — it can't see your files or launch
        Windows programs, and it's used for nothing but rendering.
      </p>

      {!status.artifacts_pinned && (
        <p className="sandbox-line">
          Note: this build has no pinned checksums, so the runtime files can't be
          verified before they're installed. Fine for development, not for a
          release.
        </p>
      )}

      {installing && progress && (
        <div className="provision-progress">
          <progress max={100} value={progress.percent} />
          <p className="sandbox-line">
            {progress.detail}… ({progress.percent}%)
          </p>
        </div>
      )}

      {error && <p className="error">{error}</p>}

      <button type="button" onClick={onInstall} disabled={!canInstall}>
        {installing ? "Installing…" : "Install the runtime"}
      </button>
    </section>
  );
}
