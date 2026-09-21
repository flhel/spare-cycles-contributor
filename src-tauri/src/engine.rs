//! Where containers actually run.
//!
//! Jobs run inside a WSL2 distro we provision, driving **Podman**. Both of those
//! are deliberate and neither is negotiable at runtime:
//!
//! - **A distro we ship**, not the contributor's own Docker Desktop, whose
//!   managed VM cannot run gVisor at all and is shared with everything else they
//!   run. Inside our own distro we control what is installed, and gVisor *is*
//!   available — verified in `docs/sandbox-verification.md`.
//! - **Podman**, because it is daemonless. WSL has no systemd to keep a daemon
//!   alive, and registering a runtime with Docker means editing `daemon.json`
//!   and restarting a root daemon — not something a consumer app should do.
//!
//! Files cross into the engine as a tar stream on stdin rather than as paths,
//! so the distro needs no access to the Windows filesystem (`/mnt/c` stays
//! disabled) and no host directory is ever named on a container command line.
//!
//! Linux and macOS will each add a variant to [`Engine`] when they are
//! implemented (see `TODO.md`); both are planned to run the same Podman+gVisor
//! environment, on the host and in a VM respectively.

use std::io::Read;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Distro used when none is configured. Overridden with `SPARECYCLES_WSL_DISTRO`.
pub const DEFAULT_WSL_DISTRO: &str = "SpareCycles";

/// Where the runtime image installs gVisor.
pub const RUNSC_PATH: &str = "/usr/local/bin/runsc";

/// The container CLI, shipped inside the runtime rather than found on the host.
pub const CONTAINER_CLI: &str = "podman";

/// How to ask for gVisor.
///
/// Podman has no runtime registry — unlike Docker, which resolves runtimes by a
/// name registered in `daemon.json` — so the runtime is named by path. That is
/// also why [`crate::sandbox`] detects gVisor by probing the binary rather than
/// asking the CLI, which would report its default runtime and answer a
/// confident "no".
pub fn gvisor_runtime_flag() -> String {
    format!("--runtime={RUNSC_PATH}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engine {
    /// Podman inside a WSL2 distro we provision, where gVisor is available.
    Wsl { distro: String },
}

impl Engine {
    /// Builds a command that runs the container CLI in the right place.
    pub fn cmd(&self) -> Command {
        match self {
            Engine::Wsl { distro } => {
                let mut cmd = Command::new("wsl");
                cmd.args(["-d", distro, "--", CONTAINER_CLI]);
                cmd
            }
        }
    }

    /// Runs an arbitrary program inside the engine's environment — used to
    /// check for gVisor, which the CLI itself won't reliably report.
    pub fn host_cmd(&self, program: &str) -> Command {
        match self {
            Engine::Wsl { distro } => {
                let mut cmd = Command::new("wsl");
                cmd.args(["-d", distro, "--", program]);
                cmd
            }
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Engine::Wsl { distro } => format!("Podman in the '{distro}' WSL2 distro"),
        }
    }
}

/// Copies one file into a container as a tar stream on stdin.
///
/// `podman cp -` takes a tar archive, which lets us hand over bytes we already
/// hold instead of naming a host path — the container gets the file without any
/// filesystem being shared with it or with the engine.
pub async fn copy_file_into(
    engine: &Engine,
    container: &str,
    dest_dir: &str,
    file_name: &str,
    contents: &[u8],
) -> Result<(), String> {
    let archive = build_tar(file_name, contents)?;

    let mut child = engine
        .cmd()
        .args(["cp", "-", &format!("{container}:{dest_dir}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start `{CONTAINER_CLI} cp`: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("`{CONTAINER_CLI} cp` gave us no stdin"))?;
    stdin
        .write_all(&archive)
        .await
        .map_err(|e| format!("could not write the input archive: {e}"))?;
    stdin
        .shutdown()
        .await
        .map_err(|e| format!("could not finish the input archive: {e}"))?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("`{CONTAINER_CLI} cp` failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{CONTAINER_CLI} cp` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Reads one file back out of a container, again as a tar stream.
pub async fn copy_file_out(
    engine: &Engine,
    container: &str,
    path_in_container: &str,
) -> Result<Vec<u8>, String> {
    let output = engine
        .cmd()
        .args(["cp", &format!("{container}:{path_in_container}"), "-"])
        .output()
        .await
        .map_err(|e| format!("could not read from the container: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "could not read {path_in_container}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    extract_single_file(&output.stdout)
}

fn build_tar(file_name: &str, contents: &[u8]) -> Result<Vec<u8>, String> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, file_name, contents)
        .map_err(|e| format!("could not build the input archive: {e}"))?;
    builder
        .into_inner()
        .map_err(|e| format!("could not finish the input archive: {e}"))
}

fn extract_single_file(archive: &[u8]) -> Result<Vec<u8>, String> {
    let mut tar = tar::Archive::new(archive);
    let mut entries = tar
        .entries()
        .map_err(|e| format!("could not read the output archive: {e}"))?;

    while let Some(entry) = entries.next() {
        let mut entry = entry.map_err(|e| format!("could not read the output archive: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| format!("could not read the rendered file: {e}"))?;
        return Ok(bytes);
    }

    Err("the container produced no file".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_survives_a_round_trip_through_tar() {
        // The bytes we stream in must be exactly the bytes that come back out —
        // a render that changed in transit would fail verification.
        let payload: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        let archive = build_tar("input.blend", &payload).unwrap();
        assert_eq!(extract_single_file(&archive).unwrap(), payload);
    }

    #[test]
    fn empty_archives_are_an_error_not_an_empty_render() {
        let empty = tar::Builder::new(Vec::new()).into_inner().unwrap();
        assert!(extract_single_file(&empty).is_err());
    }

    /// The engine never runs a container CLI on the contributor's own machine:
    /// every command goes through `wsl -d <distro>`, so the boundary cannot be
    /// skipped by an engine that happens to be installed on the host.
    #[test]
    fn every_command_runs_inside_the_distro() {
        let engine = Engine::Wsl {
            distro: "SpareCycles".into(),
        };

        let cmd = engine.cmd();
        assert_eq!(cmd.as_std().get_program(), "wsl");
        let args: Vec<_> = cmd.as_std().get_args().collect();
        assert_eq!(args, ["-d", "SpareCycles", "--", "podman"]);

        let probe = engine.host_cmd(RUNSC_PATH);
        assert_eq!(probe.as_std().get_program(), "wsl");
        let args: Vec<_> = probe.as_std().get_args().collect();
        assert_eq!(args, ["-d", "SpareCycles", "--", RUNSC_PATH]);
    }

    /// Podman has no runtime registry, so gVisor is named by path. Getting this
    /// wrong means silently running without gVisor, so it is worth pinning.
    #[test]
    fn gvisor_is_requested_by_path() {
        assert_eq!(gvisor_runtime_flag(), format!("--runtime={RUNSC_PATH}"));
        assert!(gvisor_runtime_flag().ends_with("/runsc"));
    }
}
