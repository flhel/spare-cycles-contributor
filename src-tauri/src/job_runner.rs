//! Job orchestration: fetching a job's input, handing it to the sandbox, and
//! uploading the result.
//!
//! Deliberately separate from `sandbox.rs`: everything that touches the
//! network lives here, so the isolation module stays small and reviewable.
//! The job itself never gets network access — this process downloads the
//! .blend on the host, then the container runs airgapped.

use crate::sandbox::{self, ContributorLimits, JobOutcome, RenderJobParams, SandboxStatus};
use serde::Deserialize;
use std::path::PathBuf;
use tauri::Manager;

#[derive(Debug, Deserialize)]
pub struct RunJobRequest {
    pub assignment_id: String,
    pub job_id: String,
    pub params: RenderJobParams,
    pub limits: ContributorLimits,
    /// Backend base URL, e.g. http://localhost:3000
    pub backend_url: String,
    /// Bearer token for this contributor's session.
    pub token: String,
}

#[tauri::command]
pub async fn check_sandbox() -> SandboxStatus {
    sandbox::probe_sandbox().await
}

/// Runs one assigned job end to end and reports the result to the backend.
#[tauri::command]
pub async fn run_job(app: tauri::AppHandle, request: RunJobRequest) -> Result<JobOutcome, String> {
    let scratch_dir = job_scratch_dir(&app, &request.assignment_id)?;
    tokio::fs::create_dir_all(&scratch_dir)
        .await
        .map_err(|e| format!("could not create scratch dir: {e}"))?;

    let result = run_and_report(&request, &scratch_dir).await;

    // The scratch dir holds buyer data; don't leave it lying around.
    let _ = tokio::fs::remove_dir_all(&scratch_dir).await;

    result
}

/// The full job lifecycle, minus Tauri plumbing: fetch input, render, report.
/// Public so it can be driven by the end-to-end test below.
pub async fn run_and_report(
    request: &RunJobRequest,
    scratch_dir: &PathBuf,
) -> Result<JobOutcome, String> {
    let client = reqwest::Client::new();
    let input_name = "input.blend";

    // 1. Fetch the .blend on the host, where the sandbox can't reach.
    let input = client
        .get(format!("{}/jobs/{}/input", request.backend_url, request.job_id))
        .bearer_auth(&request.token)
        .send()
        .await
        .map_err(|e| format!("could not fetch job input: {e}"))?;
    if !input.status().is_success() {
        return Err(format!("could not fetch job input: HTTP {}", input.status()));
    }
    let bytes = input
        .bytes()
        .await
        .map_err(|e| format!("could not read job input: {e}"))?;
    tokio::fs::write(scratch_dir.join(input_name), &bytes)
        .await
        .map_err(|e| format!("could not write job input: {e}"))?;

    // 2. Tell the backend we've started.
    let _ = client
        .patch(format!("{}/assignments/{}", request.backend_url, request.assignment_id))
        .bearer_auth(&request.token)
        .json(&serde_json::json!({ "status": "running" }))
        .send()
        .await;

    // 3. Render inside the sandbox.
    let host_cores = num_host_cores();
    let container_name = format!("sparecycles-job-{}", request.assignment_id);
    let outcome = sandbox::run_render_job(
        scratch_dir,
        input_name,
        &request.params,
        request.limits,
        host_cores,
        &container_name,
    )
    .await?;

    // 4. Report: upload the render, or record why it failed.
    if outcome.success {
        if let Some(path) = &outcome.output_path {
            let rendered = tokio::fs::read(path)
                .await
                .map_err(|e| format!("could not read rendered file: {e}"))?;
            let response = client
                .put(format!(
                    "{}/assignments/{}/output",
                    request.backend_url, request.assignment_id
                ))
                .bearer_auth(&request.token)
                .header("Content-Type", "application/octet-stream")
                .body(rendered)
                .send()
                .await
                .map_err(|e| format!("could not upload render: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("could not upload render: HTTP {}", response.status()));
            }
        }
    } else {
        let reason = if outcome.timed_out {
            format!("timed out after {}s", outcome.effective_caps.timeout_seconds)
        } else {
            format!(
                "render failed (exit {:?}): {}",
                outcome.exit_code, outcome.log_tail
            )
        };
        let _ = client
            .patch(format!(
                "{}/assignments/{}",
                request.backend_url, request.assignment_id
            ))
            .bearer_auth(&request.token)
            .json(&serde_json::json!({ "status": "failed", "error_message": reason }))
            .send()
            .await;
    }

    Ok(outcome)
}

fn job_scratch_dir(app: &tauri::AppHandle, assignment_id: &str) -> Result<PathBuf, String> {
    // Scratch lives under the app's own data dir — never a shared temp path.
    let base = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?;
    Ok(base.join("jobs").join(sanitize_id(assignment_id)))
}

/// Assignment ids come from the backend as uuids; refuse anything that isn't
/// one so a compromised/hostile response can't steer the scratch path.
pub(crate) fn sanitize_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect()
}

fn num_host_cores() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end run against a live backend and the job runtime. Ignored by default
    /// since it needs both; drive it with:
    ///
    /// ```text
    /// SC_BACKEND_URL=... SC_TOKEN=... SC_JOB_ID=... SC_ASSIGNMENT_ID=... \
    ///   cargo test --lib e2e_renders_a_claimed_job -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn e2e_renders_a_claimed_job() {
        let request = RunJobRequest {
            assignment_id: std::env::var("SC_ASSIGNMENT_ID").expect("SC_ASSIGNMENT_ID"),
            job_id: std::env::var("SC_JOB_ID").expect("SC_JOB_ID"),
            params: RenderJobParams {
                frame: 1,
                resolution_x: 480,
                resolution_y: 270,
                samples: 8,
                requested_caps: crate::sandbox::RequestedCaps {
                    cpu_cores: 4,
                    memory_mb: 2048,
                    timeout_seconds: 900,
                },
            },
            limits: ContributorLimits {
                max_cpu_percent: 50,
                max_memory_mb: 4096,
                max_timeout_seconds: 3600,
            },
            backend_url: std::env::var("SC_BACKEND_URL")
                .unwrap_or_else(|_| "http://localhost:3000".into()),
            token: std::env::var("SC_TOKEN").expect("SC_TOKEN"),
        };

        let scratch = std::env::temp_dir().join(format!("sc-e2e-{}", request.assignment_id));
        tokio::fs::create_dir_all(&scratch).await.unwrap();

        let outcome = run_and_report(&request, &scratch)
            .await
            .expect("job run should not error");

        println!("outcome: {outcome:#?}");
        assert!(outcome.success, "render failed: {}", outcome.log_tail);
        assert!(!outcome.timed_out);
        // 50% of the host's cores, never the 4 requested outright.
        assert!(outcome.effective_caps.cpu_cores <= num_host_cores() / 2 + 1);

        let _ = tokio::fs::remove_dir_all(&scratch).await;
    }

    #[test]
    fn sanitize_id_strips_path_traversal() {
        assert_eq!(sanitize_id("../../etc/passwd"), "etcpasswd");
        assert_eq!(
            sanitize_id("83fb2bd0-71b1-492c-9234-1f88f96367a7"),
            "83fb2bd0-71b1-492c-9234-1f88f96367a7"
        );
        assert_eq!(sanitize_id("a/b\\c:d"), "abcd");
    }
}
