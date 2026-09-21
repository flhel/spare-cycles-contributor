use crate::gpu::{self, GpuInfo};
use serde::Serialize;
use std::sync::Mutex;
use sysinfo::System;

const IDLE_CPU_THRESHOLD_PERCENT: f32 = 20.0;
const IDLE_GPU_THRESHOLD_PERCENT: f32 = 20.0;

pub struct SysInfoState(Mutex<System>);

impl SysInfoState {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        Self(Mutex::new(sys))
    }
}

#[derive(Serialize, Clone)]
pub struct SystemInfo {
    host_name: Option<String>,
    cpu_usage_percent: f32,
    cpu_core_count: usize,
    memory_used_bytes: u64,
    memory_total_bytes: u64,
    gpu: Option<GpuInfo>,
    is_idle: bool,
}

#[tauri::command]
pub async fn get_system_info(state: tauri::State<'_, SysInfoState>) -> Result<SystemInfo, String> {
    let (cpu_usage_percent, cpu_core_count, memory_used_bytes, memory_total_bytes) = {
        let mut sys = state.0.lock().map_err(|e| e.to_string())?;
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        (
            sys.global_cpu_usage(),
            sys.cpus().len(),
            sys.used_memory(),
            sys.total_memory(),
        )
    };

    let gpu = gpu::detect_gpu().await;

    // A GPU whose utilization we can't read (AMD/Intel on Windows) doesn't
    // block idleness — the CPU reading still gates job execution.
    let gpu_busy = gpu
        .as_ref()
        .and_then(|g| g.usage_percent)
        .is_some_and(|usage| usage >= IDLE_GPU_THRESHOLD_PERCENT);
    let is_idle = cpu_usage_percent < IDLE_CPU_THRESHOLD_PERCENT && !gpu_busy;

    Ok(SystemInfo {
        host_name: System::host_name(),
        cpu_usage_percent,
        cpu_core_count,
        memory_used_bytes,
        memory_total_bytes,
        gpu,
        is_idle,
    })
}
