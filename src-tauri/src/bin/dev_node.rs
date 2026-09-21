//! A headless contributor node, for testing and demos.
//!
//! Phase 4 only marks a job `verified` when two *independent* nodes agree, and
//! the backend refuses to let one account supply both results. So watching the
//! full lifecycle needs a second contributor — this is it: a different account,
//! its own node, running the same `job_runner` code the desktop app runs.
//!
//! ```text
//! cargo run --bin dev_node
//! ```
//!
//! Reads `SC_BACKEND_URL`, `SC_EMAIL`, `SC_PASSWORD`; the defaults are fine for
//! local development.

use contributor_app_lib::job_runner::{run_and_report, RunJobRequest};
use contributor_app_lib::sandbox::{ContributorLimits, RenderJobParams};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct AuthResponse {
    token: String,
}

const NODE_NAME: &str = "dev-node-2";

#[derive(Deserialize)]
struct NodeResponse {
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct Claim {
    assignment_id: String,
    job_id: String,
    params: RenderJobParams,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("\n{err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let backend =
        std::env::var("SC_BACKEND_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let email =
        std::env::var("SC_EMAIL").unwrap_or_else(|_| "contributor2@sparecycles.dev".to_string());
    let password = std::env::var("SC_PASSWORD").unwrap_or_else(|_| "password123".to_string());

    let client = reqwest::Client::new();
    let credentials = json!({ "email": email, "password": password });

    // Register, or sign in if this node has been run before.
    let token = match post::<AuthResponse>(&client, &backend, "/auth/register", &credentials, None)
        .await
    {
        Ok(auth) => {
            println!("created second contributor account {email}");
            auth.token
        }
        Err(_) => {
            let auth =
                post::<AuthResponse>(&client, &backend, "/auth/login", &credentials, None).await?;
            println!("signed in as {email}");
            auth.token
        }
    };

    // Reuse this account's node rather than registering another one each run —
    // the desktop app stores its node id for the same reason, and duplicates
    // would quietly skew any node-level reputation later on.
    let node_id = match find_node(&client, &backend, &token, NODE_NAME).await {
        Some(id) => {
            println!("reusing node {} ({NODE_NAME})", &id[..8]);
            id
        }
        None => {
            let node = post::<NodeResponse>(
                &client,
                &backend,
                "/nodes",
                &json!({ "name": NODE_NAME, "cpu_core_count": num_cores(), "max_cpu_percent": 50 }),
                Some(&token),
            )
            .await?;
            println!("registered node {} ({NODE_NAME})", &node.id[..8]);
            node.id
        }
    };


    // Claim whatever is waiting for a second opinion.
    let response = client
        .post(format!("{backend}/nodes/{node_id}/claim"))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("claim failed: {e}"))?;

    if response.status().as_u16() == 204 {
        println!("\nNothing to claim. Either no job is waiting, or this node already has them all.");
        println!("Queue one with:  npm run seed -w backend");
        return Ok(());
    }
    let claim: Claim = response
        .json()
        .await
        .map_err(|e| format!("could not read the claim: {e}"))?;
    println!(
        "claimed job {} — rendering {}x{} at {} samples",
        &claim.job_id[..8],
        claim.params.resolution_x,
        claim.params.resolution_y,
        claim.params.samples
    );

    let scratch = std::env::temp_dir().join(format!("sc-dev-node-{}", claim.assignment_id));
    tokio::fs::create_dir_all(&scratch)
        .await
        .map_err(|e| format!("could not create scratch dir: {e}"))?;

    let outcome = run_and_report(
        &RunJobRequest {
            assignment_id: claim.assignment_id,
            job_id: claim.job_id.clone(),
            params: claim.params,
            limits: ContributorLimits {
                max_cpu_percent: 50,
                max_memory_mb: 4096,
                max_timeout_seconds: 3600,
            },
            backend_url: backend.clone(),
            token: token.clone(),
        },
        &scratch,
    )
    .await?;
    let _ = tokio::fs::remove_dir_all(&scratch).await;

    println!(
        "\nrender {} — {} cores, {} MB, gVisor: {}",
        if outcome.success { "succeeded" } else { "FAILED" },
        outcome.effective_caps.cpu_cores,
        outcome.effective_caps.memory_mb,
        outcome.used_gvisor
    );
    if !outcome.success {
        println!("{}", outcome.log_tail);
        return Err("the render did not produce a result".into());
    }

    // The backend has now compared this result against the other node's. We
    // can't read the verdict here: /jobs/:id is the buyer's view, and a
    // contributor deliberately can't browse the jobs it renders. That 404 is
    // the access rule working, not a failure.
    println!("\nResult reported. The backend compares the two nodes' output hashes:");
    println!("  agreeing   -> 'verified', and payable");
    println!("  disagreeing-> 'mismatch', held for a human, nothing paid");
    println!("\nSee the verdict as the buyer:");
    println!("  npm run job-status -w backend");
    Ok(())
}

async fn find_node(
    client: &reqwest::Client,
    backend: &str,
    token: &str,
    name: &str,
) -> Option<String> {
    let nodes: Vec<NodeResponse> = client
        .get(format!("{backend}/nodes"))
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    nodes.into_iter().find(|n| n.name == name).map(|n| n.id)
}

async fn post<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    backend: &str,
    path: &str,
    body: &serde_json::Value,
    token: Option<&str>,
) -> Result<T, String> {
    let mut request = client.post(format!("{backend}{path}")).json(body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|e| format!("{path}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{path}: HTTP {}", response.status()));
    }
    response.json().await.map_err(|e| format!("{path}: {e}"))
}

fn num_cores() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}
