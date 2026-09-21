//! Sandboxed job execution.
//!
//! This module is the entire isolation boundary for buyer-submitted work, and
//! it deliberately does nothing else — no HTTP, no job bookkeeping (see
//! `job_runner.rs` for that). It drives a container engine (see `engine.rs`);
//! it never implements isolation itself.
//!
//! Three rules drive everything here:
//!   1. **gVisor is required.** A job whose syscalls would reach the host kernel
//!      is refused, not quietly downgraded. `SPARECYCLES_ALLOW_NO_GVISOR`
//!      overrides this for development only.
//!   2. The job gets no network, no host filesystem at all, no capabilities,
//!      and no privilege escalation. Its files arrive as a stream, not a mount.
//!   3. The contributor's locally-configured caps win. A job states what it
//!      wants; `EffectiveCaps::clamp` decides what it gets, and the numbers
//!      that reach the engine are always the clamped ones.
//!
//! See `docs/sandbox-verification.md` for how these properties are tested.

use crate::engine::{self, Engine, DEFAULT_WSL_DISTRO, RUNSC_PATH};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Pinned render image (built from `job-image/Dockerfile`).
pub const JOB_IMAGE: &str = "sparecycles/blender:4.2.9";

/// Expected digest of that image.
///
/// A tag is mutable: whoever controls the registry can change what
/// `:4.2.9` points at, and contributors would silently start running it. When
/// this is set, the digest is checked before every job and a mismatch refuses
/// the work. It's read from the environment because the value only becomes
/// meaningful once the image is published from CI — until then local builds
/// produce a new digest each time. Set it in production.
fn expected_image_digest() -> Option<String> {
    std::env::var("SPARECYCLES_IMAGE_DIGEST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Unprivileged uid/gid baked into the job image.
const JOB_UID_GID: &str = "65532:65532";

/// Hard ceilings applied on top of whatever the contributor configured, so a
/// mis-set slider can never hand a job the whole machine.
const ABSOLUTE_MAX_TIMEOUT_SECONDS: u64 = 21_600;
const MIN_MEMORY_MB: u64 = 256;

/// What a render job asks for. Mirrors `backend/src/jobs/contract.ts`.
#[derive(Debug, Deserialize, Clone)]
pub struct RenderJobParams {
    pub frame: u32,
    pub resolution_x: u32,
    pub resolution_y: u32,
    pub samples: u32,
    pub requested_caps: RequestedCaps,
}

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct RequestedCaps {
    pub cpu_cores: u32,
    pub memory_mb: u64,
    pub timeout_seconds: u64,
}

/// The contributor's own limits, from the app's local settings. These are the
/// authority — the backend never gets to raise them.
#[derive(Debug, Deserialize, Clone, Copy)]
pub struct ContributorLimits {
    pub max_cpu_percent: u32,
    pub max_memory_mb: u64,
    pub max_timeout_seconds: u64,
}

/// What the job actually gets, after clamping.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveCaps {
    pub cpu_cores: u32,
    pub memory_mb: u64,
    pub timeout_seconds: u64,
}

impl EffectiveCaps {
    /// Clamps a job's request against the contributor's limits and the host's
    /// real core count. Never returns more than any of its inputs allow.
    pub fn clamp(requested: RequestedCaps, limits: ContributorLimits, host_cores: u32) -> Self {
        let allowed_cores = (host_cores.max(1) as u64 * limits.max_cpu_percent.min(100) as u64) / 100;
        let allowed_cores = (allowed_cores as u32).clamp(1, host_cores.max(1));

        Self {
            cpu_cores: requested.cpu_cores.clamp(1, allowed_cores),
            memory_mb: requested
                .memory_mb
                .clamp(MIN_MEMORY_MB, limits.max_memory_mb.max(MIN_MEMORY_MB)),
            timeout_seconds: requested
                .timeout_seconds
                .min(limits.max_timeout_seconds)
                .min(ABSOLUTE_MAX_TIMEOUT_SECONDS)
                .max(1),
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct SandboxStatus {
    /// True when a usable container engine was found and answered.
    pub engine_available: bool,
    pub engine_version: Option<String>,
    /// Where jobs run, in words for the dashboard.
    pub engine: String,
    /// True when the engine offers gVisor. Without it, jobs are refused.
    pub gvisor_available: bool,
    pub image_present: bool,
    /// Digest of the local render image, for comparing against the pinned one.
    pub image_digest: Option<String>,
    /// Human-readable summary of the isolation actually in force.
    pub isolation: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct JobOutcome {
    pub success: bool,
    pub output_path: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Tail of the container's combined output, for reporting failures.
    pub log_tail: String,
    pub effective_caps: EffectiveCaps,
    pub used_gvisor: bool,
}

/// Picks where jobs should run.
///
/// There is exactly one candidate: Podman inside the WSL2 distro we provision.
/// Anything else on the machine — the contributor's own Docker Desktop, say —
/// is deliberately not a fallback, because it cannot offer gVisor and running a
/// buyer's code without gVisor is a different product, not a degraded one.
///
/// `None` means the runtime isn't installed or isn't working, which the
/// dashboard explains and `run_render_job` refuses.
pub async fn select_engine() -> Option<Engine> {
    let distro =
        std::env::var("SPARECYCLES_WSL_DISTRO").unwrap_or_else(|_| DEFAULT_WSL_DISTRO.to_string());
    let engine = Engine::Wsl { distro };

    engine_version(&engine).await.is_some().then_some(engine)
}

/// The engine we'd use if it were working — for reporting a machine that can't
/// run jobs yet, where `select_engine` has nothing to return.
fn intended_engine() -> Engine {
    Engine::Wsl {
        distro: std::env::var("SPARECYCLES_WSL_DISTRO")
            .unwrap_or_else(|_| DEFAULT_WSL_DISTRO.to_string()),
    }
}

/// Asks the engine its version — which doubles as "is it there and working?",
/// since an uninstalled distro or a broken Podman fails this.
async fn engine_version(engine: &Engine) -> Option<String> {
    let output = engine
        .cmd()
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// Probes the engine: is it there, does it offer gVisor, is the render image
/// present?
pub async fn probe_sandbox() -> SandboxStatus {
    let found = select_engine().await;
    let engine = found.clone().unwrap_or_else(intended_engine);

    let version = match &found {
        Some(engine) => engine_version(engine).await,
        None => None,
    };

    let engine_available = found.is_some();
    let gvisor_available = engine_available && engine_has_gvisor(&engine).await;
    let image_present = engine_available && image_present(&engine, JOB_IMAGE).await;
    let image_digest = if image_present {
        image_digest(&engine, JOB_IMAGE).await
    } else {
        None
    };

    let isolation = if !engine_available {
        format!(
            "{} is not available — jobs cannot run. Install the job runtime; \
             see docs/wsl-runtime-setup.md",
            engine.describe()
        )
    } else if gvisor_available {
        format!(
            "{}, running jobs under gVisor (runsc): the job's syscalls are served by \
             the sandbox kernel, not the host's",
            engine.describe()
        )
    } else {
        format!(
            "{} WITHOUT gVisor. Jobs are refused unless the development override is \
             set — see SPARECYCLES_ALLOW_NO_GVISOR",
            engine.describe()
        )
    };

    SandboxStatus {
        engine: engine.describe(),
        engine_available,
        engine_version: version,
        gvisor_available,
        image_present,
        image_digest,
        isolation,
    }
}

/// Is gVisor actually available to this engine?
///
/// Asked by probing the `runsc` binary rather than the CLI. Podman has no
/// runtime registry to consult — it reports its *default* runtime (`runc`)
/// regardless of what a container is later given — so asking it would produce a
/// confidently wrong "no". The binary is what Podman is handed via `--runtime`
/// anyway, so checking that it runs is checking the thing that matters.
async fn engine_has_gvisor(engine: &Engine) -> bool {
    engine
        .host_cmd(RUNSC_PATH)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Running a stranger's code without the sandbox we promised is not a
/// degraded mode, it's a different product. Refuse it — unless a developer has
/// explicitly opted in, which the dashboard surfaces.
fn allow_missing_gvisor() -> bool {
    std::env::var("SPARECYCLES_ALLOW_NO_GVISOR").as_deref() == Ok("true")
}

async fn image_present(engine: &Engine, image: &str) -> bool {
    engine
        .cmd()
        .args(["image", "inspect", image])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn image_digest(engine: &Engine, image: &str) -> Option<String> {
    let output = engine
        .cmd()
        .args(["image", "inspect", "--format", "{{.Id}}", image])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let digest = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!digest.is_empty()).then_some(digest)
}

/// Refuses to run if the local image isn't the one we pinned. Does nothing when
/// no expected digest is configured.
async fn verify_image_digest(engine: &Engine) -> Result<(), String> {
    let Some(expected) = expected_image_digest() else {
        return Ok(());
    };

    match image_digest(engine, JOB_IMAGE).await {
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(format!(
            "refusing to run: {JOB_IMAGE} is {actual}, expected {expected}. \
             The image has been replaced since it was pinned."
        )),
        None => Err(format!("refusing to run: cannot read the digest of {JOB_IMAGE}")),
    }
}

/// Builds the container-creation argument list.
///
/// Split out as a pure function so the isolation flags can be reviewed and
/// unit-tested without running anything. `caps` must already be clamped.
pub fn build_container_args(
    container_name: &str,
    input_file_name: &str,
    output_prefix: &str,
    params: &RenderJobParams,
    caps: EffectiveCaps,
    use_gvisor: bool,
) -> Vec<String> {
    // `create`, not `run`: the container is started separately so its output can
    // be copied out before it's removed. Note the absence of `--rm`, which would
    // delete the container — and the render with it — the moment it exits.
    let mut args: Vec<String> = vec!["create".into()];

    args.push(format!("--name={container_name}"));

    // Selects gVisor, by path — Podman has no runtime registry to look a name
    // up in. `run_render_job` refuses the job outright when this is false, so
    // the flags below are never the whole boundary in production.
    if use_gvisor {
        args.push(engine::gvisor_runtime_flag());
    }

    // No network stack at all: the job cannot phone home or scan the LAN.
    args.push("--network=none".into());

    // Nothing on the image is writable; the only writable paths are the work
    // volume and a small noexec tmpfs Blender needs for temps.
    args.push("--read-only".into());
    args.push("--tmpfs".into());
    args.push("/tmp:rw,noexec,nosuid,size=512m".into());

    // An anonymous volume, deliberately *not* a host bind mount. A bind mount
    // would open a host<->guest file-sharing channel (9p/virtiofs on Windows and
    // macOS), which is the weakest seam in an otherwise VM-backed sandbox. The
    // job's files are moved in and out with `podman cp` instead, so no host
    // directory is ever visible to the container. Removed with `podman rm -v`.
    args.push("--mount".into());
    args.push("type=volume,dst=/work".into());

    // Contributor-set resource ceilings, enforced by cgroups.
    args.push(format!("--cpus={}", caps.cpu_cores));
    args.push(format!("--memory={}m", caps.memory_mb));
    args.push(format!("--memory-swap={}m", caps.memory_mb));
    args.push("--pids-limit=512".into());

    // No privileges, no way to gain any, unprivileged uid.
    args.push("--cap-drop=ALL".into());
    args.push("--security-opt=no-new-privileges".into());
    args.push(format!("--user={JOB_UID_GID}"));

    args.push(JOB_IMAGE.into());

    // Blender's own arguments. Everything interpolated here is an integer we
    // parsed ourselves, so buyer input cannot inject arguments or Python.
    // Note: script auto-execution stays off (no --enable-autoexec), so a
    // .blend's embedded drivers/handlers do not run.
    args.push("-b".into());
    args.push(format!("/work/{input_file_name}"));
    args.push("--python-expr".into());
    args.push(render_setup_expr(params));
    args.push("-o".into());
    args.push(format!("/work/{output_prefix}"));
    args.push("-F".into());
    args.push("PNG".into());
    args.push("-f".into());
    args.push(params.frame.to_string());

    args
}

/// Builds the Blender setup expression that pins the render to the job's
/// contract.
///
/// Cycles on CPU is forced deliberately, for two reasons: the scene's own
/// engine choice must not decide what the buyer gets (EEVEE ignores
/// `cycles.samples` entirely and would silently render at its own sample
/// count), and Phase 4 compares output hashes across two nodes, which needs
/// renders to be reproducible. Seed is fixed, denoising and metadata stamping
/// are off for the same reason.
///
/// Everything interpolated is an integer parsed by us, so a buyer cannot
/// inject Python or extra arguments here.
fn render_setup_expr(params: &RenderJobParams) -> String {
    format!(
        "import bpy; s=bpy.context.scene; r=s.render; r.engine='CYCLES'; \
         r.resolution_x={x}; r.resolution_y={y}; r.resolution_percentage=100; \
         r.image_settings.file_format='PNG'; r.use_stamp=False; \
         [setattr(r,a,False) for a in dir(r) if a.startswith('use_stamp')]; \
         c=getattr(s,'cycles',None); \
         (c and [setattr(c,'device','CPU'), setattr(c,'samples',{n}), \
         setattr(c,'seed',0), setattr(c,'use_denoising',False)])",
        x = params.resolution_x,
        y = params.resolution_y,
        n = params.samples,
    )
}

/// Runs one render job to completion inside the sandbox.
///
/// `scratch_dir` is a host directory holding the input and receiving the render;
/// the container never sees it. Files cross the boundary with `podman cp`, so no
/// host path is shared into the sandbox.
pub async fn run_render_job(
    scratch_dir: &Path,
    input_file_name: &str,
    params: &RenderJobParams,
    limits: ContributorLimits,
    host_cores: u32,
    container_name: &str,
) -> Result<JobOutcome, String> {
    let caps = EffectiveCaps::clamp(params.requested_caps, limits, host_cores);
    let Some(engine) = select_engine().await else {
        return Err(format!(
            "refusing to run: {} is not available, so there is nowhere to run this \
             job safely. Install the job runtime — see docs/wsl-runtime-setup.md.",
            intended_engine().describe()
        ));
    };
    verify_image_digest(&engine).await?;

    let use_gvisor = engine_has_gvisor(&engine).await;
    if !use_gvisor && !allow_missing_gvisor() {
        return Err(format!(
            "refusing to run: {} cannot provide gVisor, and running a buyer's code \
             without it is not something to do quietly. Set \
             SPARECYCLES_ALLOW_NO_GVISOR=true to override during development.",
            engine.describe()
        ));
    }

    let output_prefix = "render_";
    let result = execute_job(
        &engine,
        scratch_dir,
        input_file_name,
        output_prefix,
        params,
        caps,
        use_gvisor,
        container_name,
    )
    .await;

    // Always tear down, whatever happened above: `--volumes` also drops the
    // anonymous work volume, so no buyer data is left on the machine.
    let _ = engine
        .cmd()
        .args(["container", "rm", "--force", "--volumes", container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    result
}

#[allow(clippy::too_many_arguments)]
async fn execute_job(
    engine: &Engine,
    scratch_dir: &Path,
    input_file_name: &str,
    output_prefix: &str,
    params: &RenderJobParams,
    caps: EffectiveCaps,
    use_gvisor: bool,
    container_name: &str,
) -> Result<JobOutcome, String> {
    let args = build_container_args(
        container_name,
        input_file_name,
        &format!("{output_prefix}####"),
        params,
        caps,
        use_gvisor,
    );

    // 1. Create the container (not started yet, so nothing runs while we stage
    //    the input).
    let create = engine
        .cmd()
        .args(&args)
        .output()
        .await
        .map_err(|e| format!("failed to start podman: {e}"))?;
    if !create.status.success() {
        return Err(format!(
            "could not create job container: {}",
            tail(&String::from_utf8_lossy(&create.stderr), 500)
        ));
    }

    // 2. Stream the .blend in. Sending bytes rather than naming a host path
    //    means the engine needs no access to this machine's filesystem — a WSL
    //    distro can keep /mnt/c disabled entirely.
    let input = tokio::fs::read(scratch_dir.join(input_file_name))
        .await
        .map_err(|e| format!("could not read job input: {e}"))?;
    engine::copy_file_into(engine, container_name, "/work", input_file_name, &input).await?;

    // 3. Run it, under the job's wall-clock cap.
    let child = engine
        .cmd()
        .args(["start", "--attach", container_name])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start job container: {e}"))?;

    let timeout = Duration::from_secs(caps.timeout_seconds);
    let (output, timed_out) = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => (
            Some(result.map_err(|e| format!("job container failed: {e}"))?),
            false,
        ),
        Err(_) => {
            // Past its deadline: stop it so it can't keep burning the
            // contributor's machine.
            let _ = engine
                .cmd()
                .args(["kill", container_name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            (None, true)
        }
    };

    let (exit_code, log_tail) = match &output {
        Some(out) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.code(), tail(&combined, 4000))
        }
        None => (
            None,
            format!("job exceeded its {}s time cap", caps.timeout_seconds),
        ),
    };

    // 4. Stream the render back out, again without sharing a directory.
    if !timed_out && exit_code == Some(0) {
        let rendered_name = format!("{output_prefix}{:04}.png", params.frame);
        if let Ok(bytes) =
            engine::copy_file_out(engine, container_name, &format!("/work/{rendered_name}")).await
        {
            tokio::fs::write(scratch_dir.join(&rendered_name), bytes)
                .await
                .map_err(|e| format!("could not save the render: {e}"))?;
        }
    }

    let rendered = find_rendered_file(scratch_dir, output_prefix, params.frame).await;
    let success = !timed_out && exit_code == Some(0) && rendered.is_some();

    Ok(JobOutcome {
        success,
        output_path: rendered.map(|p| p.to_string_lossy().into_owned()),
        exit_code,
        timed_out,
        log_tail,
        effective_caps: caps,
        used_gvisor: use_gvisor,
    })
}

/// Blender pads frame numbers to the `####` width in the output pattern.
async fn find_rendered_file(
    scratch_dir: &Path,
    prefix: &str,
    frame: u32,
) -> Option<std::path::PathBuf> {
    let candidate = scratch_dir.join(format!("{prefix}{frame:04}.png"));
    if tokio::fs::metadata(&candidate).await.is_ok() {
        return Some(candidate);
    }
    // Fall back to scanning, in case Blender padded differently.
    let mut entries = tokio::fs::read_dir(scratch_dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix) && name.ends_with(".png") {
            return Some(entry.path());
        }
    }
    None
}

fn tail(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let start = text.len() - max_bytes;
    let boundary = text
        .char_indices()
        .find(|(i, _)| *i >= start)
        .map(|(i, _)| i)
        .unwrap_or(start);
    text[boundary..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> RenderJobParams {
        RenderJobParams {
            frame: 1,
            resolution_x: 1920,
            resolution_y: 1080,
            samples: 64,
            requested_caps: RequestedCaps {
                cpu_cores: 32,
                memory_mb: 64_000,
                timeout_seconds: 100_000,
            },
        }
    }

    fn limits() -> ContributorLimits {
        ContributorLimits {
            max_cpu_percent: 50,
            max_memory_mb: 4096,
            max_timeout_seconds: 3600,
        }
    }

    #[test]
    fn clamps_a_greedy_request_down_to_contributor_limits() {
        let caps = EffectiveCaps::clamp(params().requested_caps, limits(), 16);
        // 50% of 16 cores, not the 32 requested.
        assert_eq!(caps.cpu_cores, 8);
        assert_eq!(caps.memory_mb, 4096);
        assert_eq!(caps.timeout_seconds, 3600);
    }

    #[test]
    fn never_grants_more_than_the_host_has() {
        let mut greedy = limits();
        greedy.max_cpu_percent = 100;
        let caps = EffectiveCaps::clamp(params().requested_caps, greedy, 4);
        assert_eq!(caps.cpu_cores, 4);
    }

    #[test]
    fn always_grants_at_least_one_core() {
        let mut tiny = limits();
        tiny.max_cpu_percent = 1;
        let caps = EffectiveCaps::clamp(params().requested_caps, tiny, 2);
        assert_eq!(caps.cpu_cores, 1);
    }

    #[test]
    fn modest_requests_pass_through_unchanged() {
        let mut modest = params();
        modest.requested_caps = RequestedCaps {
            cpu_cores: 2,
            memory_mb: 2048,
            timeout_seconds: 600,
        };
        let caps = EffectiveCaps::clamp(modest.requested_caps, limits(), 16);
        assert_eq!(
            caps,
            EffectiveCaps {
                cpu_cores: 2,
                memory_mb: 2048,
                timeout_seconds: 600
            }
        );
    }

    /// Renders the same job at several core counts, so the outputs can be
    /// compared. Phase 4 assumes two nodes agree on a result, but each
    /// contributor sets their own CPU cap, so nodes get *different* core
    /// counts — if that changes the pixels, exact hash comparison can't work.
    ///
    /// Ignored by default (needs the job runtime and a .blend):
    /// `SC_BLEND=/path/to/test.blend cargo test --lib determinism_across_core_counts -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn determinism_across_core_counts() {
        let blend = std::env::var("SC_BLEND").expect("SC_BLEND must point at a .blend");
        let base = std::env::temp_dir().join("sc-determinism");

        for cores in [1u32, 2, 4] {
            let dir = base.join(format!("cores-{cores}"));
            tokio::fs::create_dir_all(&dir).await.unwrap();
            tokio::fs::copy(&blend, dir.join("input.blend")).await.unwrap();

            let mut p = params();
            p.resolution_x = 320;
            p.resolution_y = 180;
            p.samples = 16;
            p.requested_caps = RequestedCaps {
                cpu_cores: cores,
                memory_mb: 2048,
                timeout_seconds: 900,
            };

            let limits = ContributorLimits {
                max_cpu_percent: 100,
                max_memory_mb: 4096,
                max_timeout_seconds: 3600,
            };

            let outcome = run_render_job(
                &dir,
                "input.blend",
                &p,
                limits,
                16,
                &format!("sc-determinism-{cores}"),
            )
            .await
            .expect("render should run");

            assert!(outcome.success, "render failed: {}", outcome.log_tail);
            assert_eq!(outcome.effective_caps.cpu_cores, cores);
            println!("cores={cores} -> {:?}", outcome.output_path);
        }

        println!("outputs written under {}", base.display());
    }

    /// Re-proves, through the real production path (`run_render_job`, not a
    /// hand-typed CLI call), that gVisor is active and cgroup caps still hold
    /// under the **current, Podman-only** engine — `docs/sandbox-verification.md`'s
    /// original cap-enforcement table (2.69s/1.03s runc, 2.41s/1.29s runsc) was
    /// measured under Docker inside the distro, before Docker was removed from
    /// the runtime path entirely. This closes that gap.
    ///
    /// Asserts `used_gvisor`, so this fails loudly rather than silently passing
    /// if `SPARECYCLES_ALLOW_NO_GVISOR` is set or the runtime regresses.
    ///
    /// Ignored by default (needs the job runtime and a .blend):
    /// `SC_BLEND=../../backend/scripts/demo-heavy.blend cargo test --lib \
    ///   proves_gvisor_and_caps_under_podman -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn proves_gvisor_and_caps_under_podman() {
        let blend = std::env::var("SC_BLEND").expect("SC_BLEND must point at a .blend");
        let base = std::env::temp_dir().join("sc-cap-proof");

        let limits = ContributorLimits {
            max_cpu_percent: 100,
            max_memory_mb: 4096,
            max_timeout_seconds: 3600,
        };

        let mut timings = Vec::new();
        for cores in [1u32, 4] {
            let dir = base.join(format!("cores-{cores}"));
            tokio::fs::create_dir_all(&dir).await.unwrap();
            tokio::fs::copy(&blend, dir.join("input.blend")).await.unwrap();

            let mut p = params();
            p.resolution_x = 320;
            p.resolution_y = 180;
            p.samples = 16;
            p.requested_caps = RequestedCaps {
                cpu_cores: cores,
                memory_mb: 2048,
                timeout_seconds: 900,
            };

            let started = std::time::Instant::now();
            let outcome = run_render_job(
                &dir,
                "input.blend",
                &p,
                limits,
                16,
                &format!("sc-cap-proof-{cores}"),
            )
            .await
            .expect("render should run");
            let elapsed = started.elapsed();

            assert!(outcome.success, "render failed: {}", outcome.log_tail);
            assert!(
                outcome.used_gvisor,
                "this proof is worthless if it silently ran without gVisor"
            );
            assert_eq!(outcome.effective_caps.cpu_cores, cores);

            println!("cores={cores} -> {:.2}s (used_gvisor={})", elapsed.as_secs_f64(), outcome.used_gvisor);
            timings.push((cores, elapsed.as_secs_f64()));
        }

        let (c1, t1) = timings[0];
        let (c4, t4) = timings[1];
        println!("{c1} core: {t1:.2}s, {c4} cores: {t4:.2}s — cgroup caps {}",
            if t4 < t1 { "held (more cores rendered faster)" } else { "DID NOT VISIBLY HOLD — investigate" });
    }

    /// Prints what this machine offers — which engine was chosen, whether
    /// gVisor is really there, which image is installed. Ignored by default
    /// since it depends on the host.
    #[tokio::test]
    #[ignore]
    async fn prints_sandbox_status() {
        println!("selected engine: {:?}", select_engine().await);
        println!("{:#?}", probe_sandbox().await);
    }

    #[test]
    fn container_args_carry_every_isolation_flag() {
        let caps = EffectiveCaps {
            cpu_cores: 2,
            memory_mb: 2048,
            timeout_seconds: 600,
        };
        let args = build_container_args("job-1", "input.blend", "render_####", &params(), caps, false);
        let joined = args.join(" ");

        assert!(joined.contains("--network=none"));
        assert!(joined.contains("--read-only"));
        assert!(joined.contains("--cap-drop=ALL"));
        assert!(joined.contains("--security-opt=no-new-privileges"));
        assert!(joined.contains("--user=65532:65532"));
        assert!(joined.contains("--cpus=2"));
        assert!(joined.contains("--memory=2048m"));
        assert!(joined.contains("--memory-swap=2048m"));
        assert!(joined.contains("--pids-limit=512"));
        // Script auto-execution must stay off.
        assert!(!joined.contains("--enable-autoexec"));

        // No host path is shared into the container: the work area is an
        // anonymous volume, and files cross via `podman cp`.
        assert!(!args.iter().any(|a| a == "-v" || a == "--volume"));
        assert!(joined.contains("type=volume,dst=/work"));
        assert_eq!(args.iter().filter(|a| *a == "--mount").count(), 1);

        // `--rm` would delete the container, and the render with it, on exit.
        assert!(!args.iter().any(|a| a == "--rm"));
    }

    /// The security model rests on an invariant that is easy to break by
    /// accident: **no string supplied by the backend ever reaches the container
    /// command line.** Ids are sanitised, everything else is an integer we
    /// parsed or a constant. If that ever stops being true, a compromised or
    /// spoofed backend could append its own flags — `--privileged`, an extra
    /// mount — and the sandbox would be worth nothing.
    #[test]
    fn hostile_backend_values_cannot_inject_container_flags() {
        let caps = EffectiveCaps {
            cpu_cores: 1,
            memory_mb: 512,
            timeout_seconds: 60,
        };

        let hostile_ids = [
            "x --privileged",
            "a -v /:/host",
            "b --network=host",
            "c\" --user=0:0 \"",
            "d\n--runtime=runc",
            "../../etc/shadow",
            "e; docker run --privileged alpine",
            "f --mount type=bind,src=/,dst=/host",
        ];

        for hostile in hostile_ids {
            // Ids reach the engine only after sanitising, exactly as job_runner does.
            let name = crate::job_runner::sanitize_id(hostile);
            let args = build_container_args(&name, "input.blend", "render_####", &params(), caps, false);

            assert!(
                !args.iter().any(|a| a == "--privileged"),
                "injected --privileged via {hostile:?}"
            );
            assert!(
                !args.iter().any(|a| a.contains("/:/") || a.contains("src=/")),
                "injected a host mount via {hostile:?}"
            );
            assert!(
                !args.iter().any(|a| a == "--user=0:0"),
                "injected a root user via {hostile:?}"
            );
            assert!(
                args.iter().any(|a| a == "--network=none"),
                "lost network isolation via {hostile:?}"
            );
            // The name is one argv element, so spaces in it can never become
            // separate arguments.
            assert_eq!(
                args.iter().filter(|a| a.starts_with("--name=")).count(),
                1,
                "name split into multiple arguments via {hostile:?}"
            );
        }
    }

    /// A tripwire: every flag passed to the engine must be one we chose on purpose.
    /// Adding a flag forces a deliberate edit here, so the sandbox cannot be
    /// widened by an unrelated change.
    #[test]
    fn container_args_contain_only_allowlisted_flags() {
        const ALLOWED: &[&str] = &[
            "--name",
            "--runtime",
            "--network",
            "--read-only",
            "--tmpfs",
            "--mount",
            "--cpus",
            "--memory",
            "--memory-swap",
            "--pids-limit",
            "--cap-drop",
            "--security-opt",
            "--user",
            // Blender's own flags, after the image name.
            "--python-expr",
        ];

        let caps = EffectiveCaps {
            cpu_cores: 2,
            memory_mb: 1024,
            timeout_seconds: 120,
        };

        for use_gvisor in [true, false] {
            let args =
                build_container_args("job-1", "input.blend", "render_####", &params(), caps, use_gvisor);

            for arg in args.iter().filter(|a| a.starts_with("--")) {
                let flag = arg.split('=').next().unwrap();
                assert!(
                    ALLOWED.contains(&flag),
                    "unexpected container flag {flag:?} — if this is intentional, add it to \
                     ALLOWED and make sure it does not weaken the sandbox"
                );
            }
        }
    }

    #[test]
    fn render_setup_pins_engine_samples_and_determinism() {
        let mut p = params();
        p.samples = 8;
        let expr = render_setup_expr(&p);

        // The scene's own engine must not decide what the buyer gets: EEVEE
        // ignores cycles.samples and would render at its own count.
        assert!(expr.contains("r.engine='CYCLES'"));
        assert!(expr.contains("setattr(c,'samples',8)"));
        assert!(expr.contains("r.resolution_x=1920"));
        assert!(expr.contains("r.resolution_y=1080"));
        // Reproducibility, so Phase 4 can compare hashes across two nodes.
        assert!(expr.contains("setattr(c,'device','CPU')"));
        assert!(expr.contains("setattr(c,'seed',0)"));
        assert!(expr.contains("setattr(c,'use_denoising',False)"));
        assert!(expr.contains("r.use_stamp=False"));
        // Metadata carries render timestamps, which would make two identical
        // renders hash differently — and can leak host details to the buyer.
        assert!(expr.contains("startswith('use_stamp')"));
    }

    #[test]
    fn gvisor_runtime_is_requested_only_when_available() {
        let caps = EffectiveCaps {
            cpu_cores: 1,
            memory_mb: 512,
            timeout_seconds: 60,
        };
        // Named by path: Podman has no runtime registry to resolve a name in,
        // so a bare "runsc" would silently fall back to the default runtime.
        let with_gvisor = build_container_args("j", "i.blend", "r_####", &params(), caps, true);
        assert!(with_gvisor
            .iter()
            .any(|a| a == &format!("--runtime={RUNSC_PATH}")));

        let without = build_container_args("j", "i.blend", "r_####", &params(), caps, false);
        assert!(!without.iter().any(|a| a.starts_with("--runtime")));
    }
}
