//! Installing the job runtime.
//!
//! `docs/wsl-runtime-setup.md` describes four manual steps: import a rootfs as a
//! WSL2 distro, stream the render image into it, and check both landed. This
//! module is those steps, done by the app.
//!
//! It is the most dangerous code in the project that isn't `sandbox.rs`. The
//! sandbox contains a job; this decides *what the sandbox is*. A tampered rootfs
//! is a sandbox that lies about isolating anything, and it would be installed
//! with the contributor's blessing. So:
//!
//!   1. **Artifacts are hashed before they are installed**, against digests
//!      compiled into this binary — never against a manifest shipped next to the
//!      files it claims to describe, which proves nothing. When no digest was
//!      pinned at build time the install still runs, but says out loud that it
//!      verified nothing.
//!   2. **Nothing installs over an existing distro.** Re-importing would replace
//!      a working runtime with whatever is on disk now, which is a downgrade
//!      attack with a progress bar. Removal stays a deliberate, manual act.
//!   3. **No caller-supplied string reaches a command line.** The distro name is
//!      validated against a strict charset, for the same reason `sandbox.rs`
//!      allowlists its flags: an argument starting with `--` is a flag, not a
//!      name.
//!
//! What this deliberately does *not* do is fetch anything. There is no registry
//! chosen yet (`TODO.md`), so artifacts are installed from a local directory —
//! the build output during development, a bundled resource in a release. Adding
//! a download step means adding transport trust to the list above, and it should
//! be reviewed as its own change.

use crate::engine::{Engine, CONTAINER_CLI, DEFAULT_WSL_DISTRO, RUNSC_PATH};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Artifact file names, as produced by `runtime/build.sh`.
pub const ROOTFS_FILE: &str = "sparecycles-runtime.tar";
pub const JOB_IMAGE_FILE: &str = "job-image.tar";

/// Expected sha256 of each artifact, baked in at compile time.
///
/// `option_env!` rather than `env!` so a development build still compiles
/// without them — but an install that couldn't check its inputs reports itself
/// as unverified rather than quietly passing. Set both when building a release:
/// the values are the ones `runtime/build.sh` prints into `manifest.txt`.
const PINNED_ROOTFS_SHA256: Option<&str> = option_env!("SPARECYCLES_RUNTIME_SHA256");
const PINNED_JOB_IMAGE_SHA256: Option<&str> = option_env!("SPARECYCLES_JOB_IMAGE_SHA256");

/// Where the runtime is installed to. WSL writes the distro's virtual disk here.
///
/// Keyed by distro name so two runtimes can never be pointed at one `ext4.vhdx`
/// — importing into an occupied directory would clobber a working install.
fn install_dir(distro: &str) -> Result<PathBuf, String> {
    let base = std::env::var("LOCALAPPDATA")
        .map_err(|_| "LOCALAPPDATA is not set, so there is nowhere to install the runtime".to_string())?;
    Ok(PathBuf::from(base).join("SpareCycles").join(distro))
}

pub fn distro_name() -> String {
    std::env::var("SPARECYCLES_WSL_DISTRO").unwrap_or_else(|_| DEFAULT_WSL_DISTRO.to_string())
}

/// Where to find the artifacts to install.
///
/// `SPARECYCLES_RUNTIME_DIR` wins, then `runtime/dist` beside the executable
/// (how a packaged build ships them), then the repository's own build output so
/// `tauri dev` works straight after `runtime/build.sh`.
pub fn artifacts_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SPARECYCLES_RUNTIME_DIR") {
        let dir = PathBuf::from(dir);
        if dir.join(ROOTFS_FILE).is_file() {
            return Some(dir);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join("runtime").join("dist"));
            // `target/debug/contributor-app.exe` -> repo root is four levels up.
            candidates.extend(
                exe_dir
                    .ancestors()
                    .take(6)
                    .map(|a| a.join("runtime").join("dist")),
            );
        }
    }

    candidates
        .into_iter()
        .find(|dir| dir.join(ROOTFS_FILE).is_file() && dir.join(JOB_IMAGE_FILE).is_file())
}

/// A distro name has to survive being handed to `wsl.exe` as an argument.
///
/// It is a single argv element, so spaces cannot split it into two arguments —
/// but a name beginning with `-` would still be read as a flag, and the name is
/// configurable. Restricting the charset is cheaper than reasoning about what
/// `wsl --import --version` would do.
fn valid_distro_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Decodes output from `wsl.exe` itself, which is UTF-16LE on Windows.
///
/// Only `wsl.exe`'s own messages need this — a Linux program's stdout comes back
/// as the bytes it wrote, which is why `sandbox.rs` reads those as plain UTF-8.
fn decode_wsl_output(bytes: &[u8]) -> String {
    let body = bytes
        .strip_prefix(&[0xFF, 0xFE][..])
        .unwrap_or(bytes);

    // A UTF-16LE run of ASCII has a zero as every second byte. Checking rather
    // than assuming keeps this working if a future WSL emits UTF-8.
    let looks_utf16 = body.len() >= 2
        && body.len().is_multiple_of(2)
        && body.iter().skip(1).step_by(2).filter(|b| **b == 0).count() > body.len() / 4;

    if looks_utf16 {
        let units: Vec<u16> = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(body).into_owned()
    }
}

/// Distros WSL knows about. `Err` means WSL itself is missing or broken.
pub async fn installed_distros() -> Result<Vec<String>, String> {
    let output = Command::new("wsl")
        .args(["--list", "--quiet"])
        .output()
        .await
        .map_err(|e| format!("could not run wsl: {e}. Is WSL installed?"))?;

    if !output.status.success() {
        return Err(format!(
            "wsl --list failed: {}",
            decode_wsl_output(&output.stderr).trim()
        ));
    }

    Ok(decode_wsl_output(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

#[derive(Debug, Serialize, Clone)]
pub struct ProvisionStatus {
    /// False when `wsl.exe` can't be run at all.
    pub wsl_available: bool,
    pub distro_name: String,
    pub distro_installed: bool,
    /// Where the artifacts were found, if they were.
    pub artifacts_dir: Option<String>,
    pub artifacts_present: bool,
    /// True when this build knows what the artifacts are supposed to hash to.
    pub artifacts_pinned: bool,
    /// True when there is nothing left to do.
    pub ready: bool,
    /// What a person should do next, in words.
    pub summary: String,
}

pub async fn provision_status() -> ProvisionStatus {
    let name = distro_name();
    let distros = installed_distros().await;
    let wsl_available = distros.is_ok();
    let distro_installed = distros
        .as_ref()
        .map(|list| list.iter().any(|d| d == &name))
        .unwrap_or(false);

    let dir = artifacts_dir();
    let artifacts_present = dir.is_some();
    let artifacts_pinned = PINNED_ROOTFS_SHA256.is_some() && PINNED_JOB_IMAGE_SHA256.is_some();

    let summary = if !wsl_available {
        "WSL is not available on this machine. The job runtime needs WSL2 — \
         install it with `wsl --install`, reboot, and try again."
            .to_string()
    } else if distro_installed {
        format!("The '{name}' runtime is installed.")
    } else if !artifacts_present {
        format!(
            "The '{name}' runtime isn't installed, and the artifacts to install it \
             ({ROOTFS_FILE}, {JOB_IMAGE_FILE}) weren't found. Build them with \
             runtime/build.sh, or point SPARECYCLES_RUNTIME_DIR at them."
        )
    } else {
        format!("Ready to install the '{name}' runtime.")
    };

    ProvisionStatus {
        wsl_available,
        distro_name: name,
        distro_installed,
        artifacts_dir: dir.map(|d| d.to_string_lossy().into_owned()),
        artifacts_present,
        artifacts_pinned,
        ready: distro_installed,
        summary,
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct ProvisionProgress {
    /// Machine-readable step id: `verify`, `import`, `load`, `check`, `done`.
    pub step: String,
    pub detail: String,
    /// Rough completion, for a progress bar. Not a time estimate.
    pub percent: u8,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProvisionOutcome {
    pub distro_name: String,
    /// False when this build had no pinned digests to check the artifacts
    /// against — the install worked, but nothing vouched for what was installed.
    pub artifacts_verified: bool,
    pub gvisor_available: bool,
    pub image_present: bool,
}

/// Hashes a file. Runs on the blocking pool: these artifacts are hundreds of
/// megabytes and hashing them would otherwise stall the async runtime.
async fn sha256_file(path: &Path) -> Result<String, String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(&path)
            .map_err(|e| format!("could not open {}: {e}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let read = file
                .read(&mut buf)
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await
    .map_err(|e| format!("hashing failed: {e}"))?
}

/// Checks one artifact against its pinned digest, if this build has one.
/// Returns whether it actually verified anything.
async fn verify_artifact(path: &Path, expected: Option<&str>) -> Result<bool, String> {
    let Some(expected) = expected.map(str::trim).filter(|e| !e.is_empty()) else {
        return Ok(false);
    };

    let actual = sha256_file(path).await?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(true)
    } else {
        Err(format!(
            "refusing to install {}: it hashes to {actual}, but this build expects \
             {expected}. The file has been replaced or corrupted since it was built.",
            path.display()
        ))
    }
}

/// Installs the runtime: verify, import, load the render image, confirm.
///
/// `progress` is called as it goes so a UI can follow along; the whole thing
/// takes minutes, most of it in the two large file operations.
pub async fn install_runtime(
    progress: impl Fn(ProvisionProgress) + Send + Sync,
) -> Result<ProvisionOutcome, String> {
    install_runtime_named(&distro_name(), progress).await
}

/// The install, with the distro named explicitly. Split out so the end-to-end
/// test can install a throwaway distro without touching a working one.
pub async fn install_runtime_named(
    name: &str,
    progress: impl Fn(ProvisionProgress) + Send + Sync,
) -> Result<ProvisionOutcome, String> {
    let name = name.to_string();
    if !valid_distro_name(&name) {
        return Err(format!(
            "refusing to install: {name:?} is not a usable distro name. Use letters, \
             digits, dot, dash or underscore."
        ));
    }

    let report = |step: &str, detail: &str, percent: u8| {
        progress(ProvisionProgress {
            step: step.to_string(),
            detail: detail.to_string(),
            percent,
        })
    };

    // Never install over a working runtime: re-importing would replace it with
    // whatever happens to be on disk now.
    let existing = installed_distros().await?;
    if existing.iter().any(|d| d == &name) {
        return Err(format!(
            "the '{name}' distro already exists, so there is nothing to install. To \
             reinstall deliberately: wsl --unregister {name} — which permanently \
             deletes it."
        ));
    }

    let dir = artifacts_dir().ok_or_else(|| {
        format!(
            "could not find {ROOTFS_FILE} and {JOB_IMAGE_FILE}. Build them with \
             runtime/build.sh, or set SPARECYCLES_RUNTIME_DIR."
        )
    })?;
    let rootfs = dir.join(ROOTFS_FILE);
    let job_image = dir.join(JOB_IMAGE_FILE);

    report("verify", "Checking the runtime artifacts", 5);
    let rootfs_verified = verify_artifact(&rootfs, PINNED_ROOTFS_SHA256).await?;
    report("verify", "Checking the render image", 20);
    let image_verified = verify_artifact(&job_image, PINNED_JOB_IMAGE_SHA256).await?;
    let artifacts_verified = rootfs_verified && image_verified;

    let target = install_dir(&name)?;
    tokio::fs::create_dir_all(&target)
        .await
        .map_err(|e| format!("could not create {}: {e}", target.display()))?;

    report("import", "Importing the WSL2 distro", 35);
    let import = Command::new("wsl")
        .arg("--import")
        .arg(&name)
        .arg(&target)
        .arg(&rootfs)
        .args(["--version", "2"])
        .output()
        .await
        .map_err(|e| format!("could not run wsl --import: {e}"))?;
    if !import.status.success() {
        return Err(format!(
            "wsl --import failed: {}",
            decode_wsl_output(&import.stderr).trim()
        ));
    }

    let engine = Engine::Wsl {
        distro: name.clone(),
    };

    report("load", "Loading the render image into the runtime", 60);
    load_job_image(&engine, &job_image).await?;

    // Confirm the two things the sandbox will refuse to run without, now rather
    // than at the first job.
    report("check", "Confirming gVisor and the render image", 90);
    let gvisor_available = engine
        .host_cmd(RUNSC_PATH)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    let image_present = engine
        .cmd()
        .args(["image", "inspect", crate::sandbox::JOB_IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    report("done", "Runtime installed", 100);

    Ok(ProvisionOutcome {
        distro_name: name,
        artifacts_verified,
        gvisor_available,
        image_present,
    })
}

/// Streams the render image into the distro's Podman.
///
/// Over stdin rather than by path, exactly as `docs/wsl-runtime-setup.md` does
/// it by hand: the distro has `automount` disabled and cannot see the Windows
/// drive the file is sitting on.
async fn load_job_image(engine: &Engine, job_image: &Path) -> Result<(), String> {
    let mut child = engine
        .cmd()
        .arg("load")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start `{CONTAINER_CLI} load`: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("`{CONTAINER_CLI} load` gave us no stdin"))?;

    let mut file = tokio::fs::File::open(job_image)
        .await
        .map_err(|e| format!("could not open {}: {e}", job_image.display()))?;

    let copied = tokio::io::copy(&mut file, &mut stdin).await;
    // Close stdin before waiting, or `podman load` sits waiting for more input.
    let _ = stdin.shutdown().await;
    drop(stdin);
    copied.map_err(|e| format!("could not stream the render image: {e}"))?;

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("`{CONTAINER_CLI} load` failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{CONTAINER_CLI} load` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn check_provision_status() -> ProvisionStatus {
    provision_status().await
}

/// Installs the runtime, emitting `provision:progress` as it goes.
#[tauri::command]
pub async fn install_job_runtime(app: tauri::AppHandle) -> Result<ProvisionOutcome, String> {
    use tauri::Emitter;
    install_runtime(move |update| {
        let _ = app.emit("provision:progress", update);
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_utf16_output_decodes_to_distro_names() {
        // Exactly what `wsl --list --quiet` writes: UTF-16LE, CRLF-separated.
        let mut bytes = Vec::new();
        for ch in "docker-desktop\r\nSpareCycles\r\n".encode_utf16() {
            bytes.extend_from_slice(&ch.to_le_bytes());
        }

        let decoded = decode_wsl_output(&bytes);
        let names: Vec<_> = decoded
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(names, ["docker-desktop", "SpareCycles"]);
    }

    #[test]
    fn a_utf16_bom_is_not_mistaken_for_a_distro_name() {
        let mut bytes = vec![0xFF, 0xFE];
        for ch in "SpareCycles\r\n".encode_utf16() {
            bytes.extend_from_slice(&ch.to_le_bytes());
        }
        assert_eq!(decode_wsl_output(&bytes).trim(), "SpareCycles");
    }

    /// If a future WSL emits UTF-8, decoding must not mangle it.
    #[test]
    fn plain_utf8_output_survives() {
        assert_eq!(decode_wsl_output(b"SpareCycles\n").trim(), "SpareCycles");
    }

    /// The same invariant `sandbox.rs` holds for container flags: a configurable
    /// string must not be able to turn into an argument to `wsl.exe`.
    #[test]
    fn a_distro_name_cannot_smuggle_in_a_flag() {
        for hostile in [
            "--version",
            "-d other",
            "SpareCycles --version 1",
            "Spare Cycles",
            "Spare;Cycles",
            "../../etc",
            "",
        ] {
            assert!(
                !valid_distro_name(hostile),
                "{hostile:?} should not be accepted as a distro name"
            );
        }

        for ok in ["SpareCycles", "spare-cycles", "spare_cycles.2", "Test1"] {
            assert!(valid_distro_name(ok), "{ok:?} should be a usable name");
        }
    }

    #[tokio::test]
    async fn an_artifact_with_no_pinned_digest_is_reported_as_unverified() {
        let path = std::env::temp_dir().join("sc-provision-unpinned.bin");
        tokio::fs::write(&path, b"anything").await.unwrap();

        assert_eq!(verify_artifact(&path, None).await, Ok(false));
        // An empty pin is the same as no pin, not a digest that matches nothing.
        assert_eq!(verify_artifact(&path, Some("  ")).await, Ok(false));

        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn a_tampered_artifact_is_refused_rather_than_installed() {
        let path = std::env::temp_dir().join("sc-provision-tampered.bin");
        tokio::fs::write(&path, b"the runtime").await.unwrap();

        // sha256("the runtime")
        let real = sha256_file(&path).await.unwrap();
        assert_eq!(verify_artifact(&path, Some(&real)).await, Ok(true));
        // Case shouldn't matter; a wrong digest must.
        assert_eq!(
            verify_artifact(&path, Some(&real.to_uppercase())).await,
            Ok(true)
        );

        let err = verify_artifact(&path, Some(&"a".repeat(64)))
            .await
            .expect_err("a mismatched digest must refuse the install");
        assert!(err.contains("refusing to install"), "unhelpful error: {err}");

        let _ = tokio::fs::remove_file(&path).await;
    }

    /// Prints what provisioning would do on this machine. Ignored by default
    /// since it depends on the host: `cargo test --lib prints_provision_status
    /// -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn prints_provision_status() {
        println!("{:#?}", provision_status().await);
    }

    /// Installs the runtime for real, under a throwaway distro name, then
    /// removes it — the only honest way to test provisioning without destroying
    /// a working runtime to do it.
    ///
    /// Ignored by default: it needs the built artifacts, moves the better part
    /// of a gigabyte, and takes minutes.
    ///
    /// `cargo test --lib e2e_installs_and_removes_a_runtime -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn e2e_installs_and_removes_a_runtime() {
        const TEST_DISTRO: &str = "SpareCyclesProvisionTest";

        // Never unregister something that was already there. If this name is
        // taken, stop rather than guess whose it is.
        let existing = installed_distros().await.expect("wsl should be available");
        assert!(
            !existing.iter().any(|d| d == TEST_DISTRO),
            "{TEST_DISTRO} already exists — remove it first, or this test would \
             delete a distro it didn't create"
        );

        let outcome = install_runtime_named(TEST_DISTRO, |update| {
            println!("[{:>3}%] {} — {}", update.percent, update.step, update.detail);
        })
        .await;

        // Clean up before asserting, so a failed install doesn't leave a distro
        // (and a gigabyte) behind.
        let removed = Command::new("wsl")
            .args(["--unregister", TEST_DISTRO])
            .output()
            .await
            .expect("wsl --unregister should run");
        let _ = tokio::fs::remove_dir_all(install_dir(TEST_DISTRO).unwrap()).await;

        let outcome = outcome.expect("the runtime should install");
        println!("{outcome:#?}");
        assert_eq!(outcome.distro_name, TEST_DISTRO);
        assert!(
            outcome.gvisor_available,
            "a freshly installed runtime must offer gVisor, or it can never run a job"
        );
        assert!(
            outcome.image_present,
            "the render image should have loaded into the new distro"
        );
        assert!(
            removed.status.success(),
            "cleanup failed: {}",
            decode_wsl_output(&removed.stderr).trim()
        );

        let after = installed_distros().await.unwrap();
        assert!(!after.iter().any(|d| d == TEST_DISTRO), "cleanup left the distro behind");
    }
}
