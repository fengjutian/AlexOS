//! Best-effort local accelerator discovery and inference provider selection.

use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(any(windows, target_os = "macos"))]
use std::process::Command;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum ComputeProvider {
    Cpu,
    Cuda,
    DirectMl,
    CoreMl,
    Rocm,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HardwareDevice {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub provider: ComputeProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HardwareProfile {
    pub logical_cpus: usize,
    pub devices: Vec<HardwareDevice>,
    pub providers: Vec<ComputeProvider>,
}

pub fn discover() -> HardwareProfile {
    let logical_cpus = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let mut devices = vec![HardwareDevice {
        id: "cpu:0".into(),
        name: format!("{} CPU", std::env::consts::ARCH),
        kind: "cpu".into(),
        provider: ComputeProvider::Cpu,
        memory_mb: physical_memory_mb(),
    }];
    discover_accelerators(&mut devices);
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    devices.dedup_by(|a, b| a.id == b.id);
    let mut providers = devices
        .iter()
        .map(|device| device.provider)
        .collect::<Vec<_>>();
    providers.sort();
    providers.dedup();
    HardwareProfile {
        logical_cpus,
        devices,
        providers,
    }
}

#[cfg(windows)]
fn discover_accelerators(devices: &mut Vec<HardwareDevice>) {
    // Fixed command and arguments only. CSV output is treated as untrusted data.
    let output = Command::new("wmic")
        .args([
            "path",
            "win32_VideoController",
            "get",
            "Name,AdapterRAM",
            "/format:csv",
        ])
        .output();
    let Ok(output) = output else { return };
    if !output.status.success() {
        return;
    }
    for (index, line) in String::from_utf8_lossy(&output.stdout).lines().enumerate() {
        let columns = line.trim().split(',').collect::<Vec<_>>();
        if columns.len() < 3 || columns[1].eq_ignore_ascii_case("AdapterRAM") {
            continue;
        }
        let name = columns[2].trim();
        if name.is_empty() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        let memory_mb = columns[1]
            .trim()
            .parse::<u64>()
            .ok()
            .map(|bytes| bytes / 1024 / 1024);
        devices.push(HardwareDevice {
            id: format!("gpu:{index}:directml"),
            name: name.into(),
            kind: "gpu".into(),
            provider: ComputeProvider::DirectMl,
            memory_mb,
        });
        if lower.contains("nvidia") {
            devices.push(HardwareDevice {
                id: format!("gpu:{index}:cuda"),
                name: name.into(),
                kind: "gpu".into(),
                provider: ComputeProvider::Cuda,
                memory_mb,
            });
        }
    }
    if let Ok(output) = Command::new("wmic")
        .args(["path", "Win32_PnPEntity", "get", "Name", "/format:list"])
        .output()
        && output.status.success()
    {
        for (index, name) in String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().strip_prefix("Name="))
            .filter(|name| {
                let lower = name.to_ascii_lowercase();
                lower.contains("neural") || lower.contains(" npu") || lower.starts_with("npu")
            })
            .enumerate()
        {
            devices.push(HardwareDevice {
                id: format!("npu:{index}:directml"),
                name: name.into(),
                kind: "npu".into(),
                provider: ComputeProvider::DirectMl,
                memory_mb: None,
            });
        }
    }
}

#[cfg(target_os = "linux")]
fn discover_accelerators(devices: &mut Vec<HardwareDevice>) {
    if Path::new("/dev/nvidiactl").exists() {
        devices.push(HardwareDevice {
            id: "gpu:0:cuda".into(),
            name: "NVIDIA GPU".into(),
            kind: "gpu".into(),
            provider: ComputeProvider::Cuda,
            memory_mb: None,
        });
    }
    if Path::new("/dev/kfd").exists() {
        devices.push(HardwareDevice {
            id: "gpu:0:rocm".into(),
            name: "AMD GPU".into(),
            kind: "gpu".into(),
            provider: ComputeProvider::Rocm,
            memory_mb: None,
        });
    }
    if Path::new("/dev/accel").exists() {
        devices.push(HardwareDevice {
            id: "npu:0".into(),
            name: "Linux accelerator".into(),
            kind: "npu".into(),
            provider: ComputeProvider::Cpu,
            memory_mb: None,
        });
    }
}

#[cfg(target_os = "macos")]
fn discover_accelerators(devices: &mut Vec<HardwareDevice>) {
    devices.push(HardwareDevice {
        id: "ane:0:coreml".into(),
        name: "Apple Core ML device".into(),
        kind: "gpu-npu".into(),
        provider: ComputeProvider::CoreMl,
        memory_mb: physical_memory_mb(),
    });
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn discover_accelerators(_: &mut Vec<HardwareDevice>) {}

#[cfg(target_os = "linux")]
fn physical_memory_mb() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb = text.lines().find_map(|line| {
        line.strip_prefix("MemTotal:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    Some(kb / 1024)
}

#[cfg(windows)]
fn physical_memory_mb() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe {
        GlobalMemoryStatusEx(&mut status).ok()?;
    }
    Some(status.ullTotalPhys / 1024 / 1024)
}

#[cfg(target_os = "macos")]
fn physical_memory_mb() -> Option<u64> {
    let output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .ok()
        .map(|bytes| bytes / 1024 / 1024)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn physical_memory_mb() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_always_exposes_cpu() {
        let profile = discover();
        assert!(profile.logical_cpus >= 1);
        assert!(profile.providers.contains(&ComputeProvider::Cpu));
        assert!(profile.devices.iter().any(|device| device.kind == "cpu"));
    }
}
