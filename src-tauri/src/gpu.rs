//! GPU detection.
//!
//! Vendor-agnostic on purpose: the target audience is gaming PCs, which are as
//! likely to be AMD as NVIDIA. `nvidia-smi` gives name *and* live utilization,
//! but there's no equivalent for AMD/Intel on Windows, so those report a name
//! with `usage_percent: None` rather than pretending to be undetected. Live
//! utilization for them needs the PDH performance counters — see `TODO.md`.

use serde::Serialize;
use tokio::sync::OnceCell;

#[derive(Serialize, Clone, Debug)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: String,
    /// `None` when the GPU is known but its utilization can't be read here.
    pub usage_percent: Option<f32>,
}

/// The adapter name doesn't change while the app runs, and querying it costs a
/// subprocess, so it's resolved once and reused.
static CACHED_ADAPTER: OnceCell<Option<(String, String)>> = OnceCell::const_new();

pub async fn detect_gpu() -> Option<GpuInfo> {
    if let Some(gpu) = query_nvidia().await {
        return Some(gpu);
    }

    #[cfg(target_os = "linux")]
    if let Some(gpu) = query_linux_sysfs().await {
        return Some(gpu);
    }

    // Known adapter, unknown utilization.
    let (name, vendor) = CACHED_ADAPTER.get_or_init(query_adapter_name).await.clone()?;
    Some(GpuInfo {
        name,
        vendor,
        usage_percent: None,
    })
}

/// NVIDIA: name and utilization in one call, when the driver tooling is present.
async fn query_nvidia() -> Option<GpuInfo> {
    let output = tokio::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let mut parts = text.lines().next()?.split(',').map(str::trim);
    let name = parts.next()?.to_string();
    let usage_percent: f32 = parts.next()?.parse().ok()?;

    Some(GpuInfo {
        name,
        vendor: "NVIDIA".to_string(),
        usage_percent: Some(usage_percent),
    })
}

/// AMD on Linux exposes utilization directly in sysfs, no tooling required.
#[cfg(target_os = "linux")]
async fn query_linux_sysfs() -> Option<GpuInfo> {
    let mut entries = tokio::fs::read_dir("/sys/class/drm").await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }

        let device = entry.path().join("device");
        let Ok(busy) = tokio::fs::read_to_string(device.join("gpu_busy_percent")).await else {
            continue;
        };
        let Ok(usage) = busy.trim().parse::<f32>() else {
            continue;
        };

        let vendor_id = tokio::fs::read_to_string(device.join("vendor"))
            .await
            .unwrap_or_default();
        let vendor = match vendor_id.trim() {
            "0x1002" => "AMD",
            "0x8086" => "Intel",
            "0x10de" => "NVIDIA",
            _ => "Unknown",
        };

        return Some(GpuInfo {
            name: format!("{vendor} GPU ({name})"),
            vendor: vendor.to_string(),
            usage_percent: Some(usage),
        });
    }
    None
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
async fn query_linux_sysfs() -> Option<GpuInfo> {
    None
}

/// Adapter name/vendor for AMD and Intel cards, where there's no vendor CLI.
#[cfg(target_os = "windows")]
async fn query_adapter_name() -> Option<(String, String)> {
    let output = tokio::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_VideoController | Select-Object -First 1 | \
             ForEach-Object { \"$($_.Name)|$($_.AdapterCompatibility)\" }",
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    let (name, vendor) = line.split_once('|')?;
    if name.is_empty() {
        return None;
    }

    Some((name.to_string(), normalize_vendor(vendor)))
}

#[cfg(not(target_os = "windows"))]
async fn query_adapter_name() -> Option<(String, String)> {
    None
}

fn normalize_vendor(raw: &str) -> String {
    let lower = raw.to_lowercase();
    // NVIDIA and Intel are checked first: "ati" is a substring of
    // "NVIDIA Corporation" ("corpor-ati-on"), so a loose AMD check wins wrongly.
    if lower.contains("nvidia") {
        "NVIDIA".to_string()
    } else if lower.contains("intel") {
        "Intel".to_string()
    } else if lower.contains("advanced micro") || lower.contains("amd") || lower.contains("ati technologies") {
        "AMD".to_string()
    } else if raw.trim().is_empty() {
        "Unknown".to_string()
    } else {
        raw.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_vendor;

    /// Prints what this machine actually reports. Ignored by default since the
    /// result depends on the host's hardware.
    #[tokio::test]
    #[ignore]
    async fn prints_detected_gpu() {
        println!("{:#?}", super::detect_gpu().await);
    }

    #[test]
    fn normalizes_vendor_strings() {
        assert_eq!(normalize_vendor("Advanced Micro Devices, Inc."), "AMD");
        assert_eq!(normalize_vendor("NVIDIA Corporation"), "NVIDIA");
        assert_eq!(normalize_vendor("Intel Corporation"), "Intel");
        assert_eq!(normalize_vendor("ATI Technologies Inc."), "AMD");
        assert_eq!(normalize_vendor(""), "Unknown");
    }
}
