//! Loads local host/device profiles from `config/`.
//!
//! Nothing machine- or device-specific belongs in source. Anyone cloning this
//! repo has different hardware, a different network, and possibly several
//! phones, so all of that lives in gitignored TOML under `config/` and is read
//! at runtime.
//!
//! Device selection order: `PALMTOP_DEVICE` env var -> `config/active` ->
//! an error listing what is available.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

// ---------------------------------------------------------------- host

#[derive(Debug, Deserialize)]
pub struct HostConfig {
    pub host: HostSection,
    pub gpu: GpuSection,
    #[serde(default)]
    pub encode: EncodeSection,
}

#[derive(Debug, Deserialize)]
pub struct HostSection {
    /// Empty means "auto-detect the primary interface address".
    #[serde(default)]
    pub ip: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

#[derive(Debug, Deserialize)]
pub struct GpuSection {
    pub vaapi_render_node: String,
}

#[derive(Debug, Deserialize)]
pub struct EncodeSection {
    #[serde(default = "default_codec")]
    pub codec: String,
    #[serde(default = "default_qp")]
    pub qp: u32,
    #[serde(default = "default_fps")]
    pub fps: u32,
}

impl Default for EncodeSection {
    fn default() -> Self {
        Self { codec: default_codec(), qp: default_qp(), fps: default_fps() }
    }
}

fn default_port() -> u16 { 9999 }
fn default_codec() -> String { "h264_vaapi".into() }
fn default_qp() -> u32 { 24 }
fn default_fps() -> u32 { 30 }

impl HostConfig {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("host.toml");
        if !path.exists() {
            bail!(
                "missing {}\n\nCreate it from the template:\n    cp config/host.example.toml config/host.toml\n\
                 Then edit it, or run ./scripts/probe-host.sh to generate one.",
                path.display()
            );
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    /// Configured IP, or the primary interface address if left blank.
    pub fn resolved_ip(&self) -> Result<String> {
        if !self.host.ip.is_empty() {
            return Ok(self.host.ip.clone());
        }
        detect_primary_ip().context(
            "could not auto-detect host IP -- set `ip` explicitly in config/host.toml",
        )
    }
}

// ---------------------------------------------------------------- device

#[derive(Debug, Deserialize)]
pub struct DeviceConfig {
    pub device: DeviceSection,
    pub adb: AdbSection,
    #[serde(default)]
    pub display: DisplaySection,
    #[serde(default)]
    pub decoder: DecoderSection,
    #[serde(default)]
    pub limits: LimitsSection,
}

#[derive(Debug, Deserialize)]
pub struct DeviceSection {
    pub name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub sdk: u32,
}

#[derive(Debug, Deserialize)]
pub struct AdbSection {
    pub serial: String,
    #[serde(default)]
    pub ip: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct DisplaySection {
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub density: u32,
    #[serde(default)]
    pub refresh_hz: f64,
}

#[derive(Debug, Default, Deserialize)]
pub struct DecoderSection {
    /// Empty means "let the client auto-select at runtime".
    #[serde(default)]
    pub h264: String,
    #[serde(default)]
    pub h265: String,
}

#[derive(Debug, Deserialize)]
pub struct LimitsSection {
    #[serde(default = "default_w")]
    pub max_width: u32,
    #[serde(default = "default_h")]
    pub max_height: u32,
    #[serde(default = "default_fps")]
    pub max_fps: u32,
}

impl Default for LimitsSection {
    fn default() -> Self {
        Self { max_width: default_w(), max_height: default_h(), max_fps: default_fps() }
    }
}

fn default_w() -> u32 { 1920 }
fn default_h() -> u32 { 1080 }

impl DeviceConfig {
    /// Loads the active device: `PALMTOP_DEVICE` -> `config/active` -> error.
    pub fn load() -> Result<Self> {
        let dir = config_dir()?.join("devices");
        let name = match std::env::var("PALMTOP_DEVICE") {
            Ok(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => {
                let active = config_dir()?.join("active");
                if !active.exists() {
                    bail!("{}", no_device_help(&dir));
                }
                std::fs::read_to_string(&active)?.trim().to_string()
            }
        };
        Self::load_named(&name)
    }

    pub fn load_named(name: &str) -> Result<Self> {
        let path = config_dir()?.join("devices").join(format!("{name}.toml"));
        if !path.exists() {
            bail!(
                "no device profile '{name}' at {}\n{}",
                path.display(),
                no_device_help(&config_dir()?.join("devices"))
            );
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    /// Device profiles present, excluding the committed template.
    pub fn available() -> Vec<String> {
        let Ok(dir) = config_dir().map(|d| d.join("devices")) else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension()? != "toml" {
                    return None;
                }
                let stem = p.file_stem()?.to_str()?.to_string();
                (stem != "example").then_some(stem)
            })
            .collect();
        out.sort();
        out
    }
}

fn no_device_help(dir: &Path) -> String {
    let available = DeviceConfig::available();
    let listing = if available.is_empty() {
        "  (none yet)".to_string()
    } else {
        available.iter().map(|n| format!("  {n}")).collect::<Vec<_>>().join("\n")
    };
    format!(
        "No device selected.\n\nAvailable profiles in {}:\n{listing}\n\n\
         Pick one with either:\n    echo <name> > config/active\n    PALMTOP_DEVICE=<name> <command>\n\n\
         Create one with:\n    ./scripts/probe-device.sh <adb-serial>",
        dir.display()
    )
}

// ---------------------------------------------------------------- helpers

/// Locates `config/` by walking up from the executable or CWD, so binaries work
/// from anywhere in the tree.
pub fn config_dir() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("PALMTOP_CONFIG_DIR") {
        return Ok(PathBuf::from(explicit));
    }
    let mut dir = std::env::current_dir().context("get cwd")?;
    loop {
        let candidate = dir.join("config");
        if candidate.join("host.example.toml").exists() {
            return Ok(candidate);
        }
        if !dir.pop() {
            bail!(
                "could not locate the config/ directory -- run from inside the repo, \
                 or set PALMTOP_CONFIG_DIR"
            );
        }
    }
}

/// Primary outbound IPv4 address, found by asking the routing table which
/// source address it would use (no packets are actually sent).
fn detect_primary_ip() -> Result<String> {
    use std::net::UdpSocket;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.connect("8.8.8.8:53")?;
    Ok(sock.local_addr()?.ip().to_string())
}
