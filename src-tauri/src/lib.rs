pub mod engine;
pub mod gpu;
pub mod job_runner;
pub mod provision;
pub mod sandbox;
pub mod system_info;

use job_runner::{check_sandbox, run_job};
use provision::{check_provision_status, install_job_runtime};
use system_info::{get_system_info, SysInfoState};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(SysInfoState::new())
        .invoke_handler(tauri::generate_handler![
            get_system_info,
            check_sandbox,
            check_provision_status,
            install_job_runtime,
            run_job
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
