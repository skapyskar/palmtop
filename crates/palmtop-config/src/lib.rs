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
    #[serde(default)]
    pub pairing: PairingSection,
}

#[derive(Debug, Default, Deserialize)]
pub struct PairingSection {
    /// Shared secret a client must present in `Hello` to be accepted --
    /// see palmtopd/src/pairing.rs. Generated on first run and appended to
    /// `config/host.toml` if missing (never generated fresh on every start,
    /// or every previously-paired client would be locked out each restart).
    #[serde(default)]
    pub token: String,
    /// Static Noise (X25519) keypair, hex-encoded. Same generate-once,
    /// persist-forever pattern as `token` -- see `palmtop_proto::noise` for
    /// why the client needs to TOFU-pin the *public* half ahead of time, and
    /// why regenerating this on every restart would break every previously
    /// paired client, not just prompt a re-scan.
    #[serde(default)]
    pub noise_private_key: String,
    #[serde(default)]
    pub noise_public_key: String,
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
    /// VA-API frame-pipelining depth. The Phase 0 encode spike measured
    /// *throughput* (120fps batch) and never per-frame latency; VA-API
    /// buffers several frames internally by default to hit that throughput,
    /// which is invisible in a batch benchmark but shows up directly as
    /// input-to-screen lag. Default 1 trades throughput headroom (we have
    /// 4x to spare against a 30fps target) for latency, which is the
    /// correct trade for an interactive control loop.
    #[serde(default = "default_async_depth")]
    pub async_depth: u32,
}

impl Default for EncodeSection {
    fn default() -> Self {
        Self {
            codec: default_codec(),
            qp: default_qp(),
            fps: default_fps(),
            async_depth: default_async_depth(),
        }
    }
}

fn default_port() -> u16 { 9999 }
fn default_codec() -> String { "h264_vaapi".into() }
fn default_qp() -> u32 { 24 }
fn default_fps() -> u32 { 30 }
fn default_async_depth() -> u32 { 1 }

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
        let mut cfg: HostConfig =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

        // Appended as raw text, not re-serialized, so existing comments and
        // formatting survive. TOML forbids redefining a `[section]` header,
        // so the header is only emitted once even if both the token and the
        // Noise keypair turn out to be missing (true on a fresh file) --
        // bare `key = value` lines appended afterward, with no new header,
        // correctly attach to whichever `[pairing]` table is already open,
        // which is always the last section in files this function writes.
        let mut appended = String::new();
        // A naive text.contains("[pairing]") is fooled by host.example.toml's
        // own explanatory comment ("# [pairing] is NOT listed here on
        // purpose..."), which contains that exact substring despite there
        // being no real [pairing] table. That false positive suppressed the
        // header on every fresh install, so the generated token/keypair
        // silently attached to whatever table happened to be physically last
        // in the file (`[encode]`) instead -- never read back as `pairing`
        // on the next load, so every restart "discovered" a missing token
        // and appended another one, until two collided as a duplicate key
        // and crashed the daemon for good. Match only a real section header
        // line, not the substring anywhere in the text.
        let has_pairing_header = text.lines().any(|line| line.trim() == "[pairing]");
        let open_header = |appended: &mut String| {
            if !has_pairing_header && appended.is_empty() {
                appended.push_str("\n[pairing]\n");
            }
        };

        if cfg.pairing.token.is_empty() {
            let token = generate_token()?;
            open_header(&mut appended);
            appended.push_str(&format!(
                "# Generated on first run -- required in Hello for a client to be accepted.\ntoken = \"{token}\"\n"
            ));
            cfg.pairing.token = token;
        }

        if cfg.pairing.noise_private_key.is_empty() || cfg.pairing.noise_public_key.is_empty() {
            let (priv_key, pub_key) = palmtop_proto::NoiseTransport::generate_keypair()
                .context("generate noise keypair")?;
            let priv_hex = palmtop_proto::noise::to_hex(&priv_key);
            let pub_hex = palmtop_proto::noise::to_hex(&pub_key);
            open_header(&mut appended);
            appended.push_str(&format!(
                "# Static Noise keypair -- the public half also goes into the QR/pairing\n\
                 # info so clients can TOFU-pin it. See palmtop-proto::noise.\n\
                 noise_private_key = \"{priv_hex}\"\nnoise_public_key = \"{pub_hex}\"\n"
            ));
            cfg.pairing.noise_private_key = priv_hex;
            cfg.pairing.noise_public_key = pub_hex;
        }

        if !appended.is_empty() {
            let mut updated = text;
            if !updated.ends_with('\n') {
                updated.push('\n');
            }
            updated.push_str(&appended);
            std::fs::write(&path, updated)
                .with_context(|| format!("persist generated pairing info to {}", path.display()))?;
        }

        Ok(cfg)
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

    /// The DRM render node to hardware-encode on: the configured one if it can
    /// actually encode, otherwise the first node that can.
    ///
    /// The template's `/dev/dri/renderD128` is a guess, and on hybrid-GPU
    /// laptops it is frequently the wrong guess -- the nodes enumerate in an
    /// order that depends on which driver bound first, which is not stable
    /// across machines. Getting it wrong used to be silent and awful to
    /// diagnose: ffmpeg fails to initialise VA-API and exits immediately, the
    /// capture pipeline keeps running, and the phone sits on a blank screen
    /// forever with no error at either end. Probing turns that into either a
    /// working stream or a loud, specific message.
    ///
    /// Kept as its own method (rather than folded entirely into
    /// `resolved_encode_backend`) because it is also what `--doctor` calls to
    /// report on VA-API specifically, and because it bails loudly on failure,
    /// which is right for a direct diagnostic but wrong for
    /// `resolved_encode_backend`'s "fall through to the next backend" needs --
    /// see `working_vaapi_node`, which the two share.
    pub fn resolved_render_node(&self) -> Result<String> {
        match working_vaapi_node(&self.gpu.vaapi_render_node, &self.encode.codec) {
            Some(node) => Ok(node),
            None if render_nodes().is_empty() => bail!(
                "no DRM render nodes found under /dev/dri -- this machine has no GPU available \
                 for hardware encode. Run `palmtopd --doctor` for details."
            ),
            None => bail!(
                "none of the available render nodes ({}) can hardware-encode {}. Run \
                 `palmtopd --doctor` for the full diagnosis and how to fix it.",
                render_nodes().join(", "),
                self.encode.codec
            ),
        }
    }

    /// The encoder to actually run: VA-API if any GPU node can do it, else
    /// NVENC, else software x264 -- whichever this specific machine can
    /// really do, established the same way as the render node is (by
    /// encoding through it), not by guessing from what hardware is nominally
    /// present.
    ///
    /// A machine with no usable VA-API node is not necessarily a machine that
    /// cannot run Palmtop at all: NVIDIA GPUs frequently expose NVENC without
    /// a usable VA-API path (the open NVIDIA VA-API shim is inconsistently
    /// packaged), and even a machine with no GPU encoder at all can still
    /// encode in software -- slower, and a real CPU cost, but a working
    /// stream beats `--doctor` printing "will not work on this machine" for
    /// someone who would have been fine with the software path.
    ///
    /// `encode.codec` in host.toml can pin one of these explicitly
    /// (`h264_nvenc`, `libx264`); anything else (including the template's
    /// default `h264_vaapi`) means "figure it out", in the same
    /// narrow-not-widen spirit as `resolved_render_node`.
    pub fn resolved_encode_backend(&self) -> Result<EncodeBackend> {
        let configured = self.encode.codec.trim();

        if configured == EncodeBackend::Nvenc.codec_name() {
            if nvenc_encodes() {
                return Ok(EncodeBackend::Nvenc);
            }
            eprintln!(
                "[gpu] configured codec {configured} does not work on this machine -- \
                 falling back to auto-detection."
            );
        } else if configured == EncodeBackend::Software.codec_name() {
            if software_encodes() {
                return Ok(EncodeBackend::Software);
            }
            eprintln!(
                "[gpu] configured codec {configured} does not work on this machine (is ffmpeg \
                 built with libx264?) -- falling back to auto-detection."
            );
        } else if configured == EncodeBackend::Qsv.codec_name() {
            if qsv_encodes() {
                return Ok(EncodeBackend::Qsv);
            }
            eprintln!(
                "[gpu] configured codec {configured} does not work on this machine -- \
                 falling back to auto-detection."
            );
        } else if configured == EncodeBackend::Amf.codec_name() {
            if amf_encodes() {
                return Ok(EncodeBackend::Amf);
            }
            eprintln!(
                "[gpu] configured codec {configured} does not work on this machine -- \
                 falling back to auto-detection."
            );
        }

        // Anything else, including the unconfigured default (h264_vaapi), is
        // "figure it out". The order tried is platform-specific because the
        // *cheapest-when-it-works* backend differs: VA-API on Linux, Quick
        // Sync/NVENC/AMF on Windows (all GPU vendors get a real hardware
        // path there, checked in an arbitrary but stable order since there's
        // no single "cheapest" answer across Intel/NVIDIA/AMD).
        #[cfg(target_os = "linux")]
        {
            if let Some(node) = working_vaapi_node(&self.gpu.vaapi_render_node, "h264_vaapi") {
                return Ok(EncodeBackend::Vaapi { render_node: node });
            }
            if nvenc_encodes() {
                eprintln!("[gpu] no VA-API render node can encode -- using NVENC instead.");
                return Ok(EncodeBackend::Nvenc);
            }
            if software_encodes() {
                eprintln!(
                    "[gpu] no hardware encoder (VA-API or NVENC) is available -- falling back \
                     to software encoding. This costs real CPU and may not sustain the target \
                     framerate at high resolutions; run `palmtopd --doctor` for the full \
                     picture."
                );
                return Ok(EncodeBackend::Software);
            }
            bail!(
                "no working video encoder found on this machine -- tried VA-API, NVENC, and \
                 software (libx264) encoding. Run `palmtopd --doctor` for the full diagnosis."
            );
        }
        #[cfg(windows)]
        {
            if qsv_encodes() {
                eprintln!("[gpu] using Intel Quick Sync (QSV).");
                return Ok(EncodeBackend::Qsv);
            }
            if nvenc_encodes() {
                eprintln!("[gpu] no Quick Sync device found -- using NVENC instead.");
                return Ok(EncodeBackend::Nvenc);
            }
            if amf_encodes() {
                eprintln!("[gpu] no Quick Sync or NVENC device found -- using AMD AMF instead.");
                return Ok(EncodeBackend::Amf);
            }
            if software_encodes() {
                eprintln!(
                    "[gpu] no hardware encoder (QSV, NVENC, or AMF) is available -- falling \
                     back to software encoding. This costs real CPU and may not sustain the \
                     target framerate at high resolutions; run `palmtopd --doctor` for the \
                     full picture."
                );
                return Ok(EncodeBackend::Software);
            }
            bail!(
                "no working video encoder found on this machine -- tried Quick Sync, NVENC, \
                 AMF, and software (libx264) encoding. Run `palmtopd --doctor` for the full \
                 diagnosis."
            );
        }
    }
}

/// Which real encoder this machine will run through, and what `encode::spawn`
/// needs to know to build the right ffmpeg invocation for it. See
/// `HostConfig::resolved_encode_backend`.
///
/// `Vaapi` is Linux-only (it names a `/dev/dri` render node); `Qsv` and `Amf`
/// are Windows-only (Intel Quick Sync and AMD AMF respectively -- ffmpeg
/// exposes both as ordinary encoders, not through a hwaccel device handle,
/// the same shape as `Nvenc`). `Nvenc` and `Software` are the two backends
/// that exist on both platforms unchanged, since NVENC and libx264 are the
/// same ffmpeg encoders either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeBackend {
    Vaapi { render_node: String },
    Nvenc,
    Qsv,
    Amf,
    Software,
}

impl EncodeBackend {
    pub fn codec_name(&self) -> &'static str {
        match self {
            EncodeBackend::Vaapi { .. } => "h264_vaapi",
            EncodeBackend::Nvenc => "h264_nvenc",
            EncodeBackend::Qsv => "h264_qsv",
            EncodeBackend::Amf => "h264_amf",
            EncodeBackend::Software => "libx264",
        }
    }
}

impl std::fmt::Display for EncodeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeBackend::Vaapi { render_node } => write!(f, "VA-API on {render_node}"),
            EncodeBackend::Nvenc => write!(f, "NVENC"),
            EncodeBackend::Qsv => write!(f, "Intel Quick Sync (QSV)"),
            EncodeBackend::Amf => write!(f, "AMD AMF"),
            EncodeBackend::Software => write!(f, "software (libx264)"),
        }
    }
}

/// The configured VA-API node if it can really encode `codec`, else the first
/// available node that can, else `None`. Shared by `resolved_render_node`
/// (which turns `None` into a specific bail) and `resolved_encode_backend`
/// (which turns it into "try the next backend").
fn working_vaapi_node(configured: &str, codec: &str) -> Option<String> {
    let configured = configured.trim();
    if !configured.is_empty() && node_can_encode(configured, codec) {
        return Some(configured.to_string());
    }
    let node = render_nodes().into_iter().find(|n| node_can_encode(n, codec));
    if let Some(node) = &node {
        if configured.is_empty() {
            eprintln!("[gpu] auto-detected render node: {node}");
        } else {
            eprintln!(
                "[gpu] configured render node {configured} cannot encode {codec} -- using \
                 {node} instead. Set `vaapi_render_node = \"{node}\"` in host.toml to silence \
                 this."
            );
        }
    }
    node
}

/// Encodes a few generated frames through NVENC and reports whether ffmpeg
/// succeeded -- the same "prove it by doing it" approach as `node_can_encode`,
/// for the same reason: NVENC support depends on the installed driver and
/// ffmpeg build in ways that are not worth trying to infer from `lspci`.
pub fn nvenc_encodes() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .args([
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=0.2",
            "-c:v", "h264_nvenc",
            "-f", "null", "-",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Same, for software libx264. Almost always succeeds if ffmpeg has libx264
/// compiled in at all, but "almost always" is exactly the gap a real probe
/// closes and a version check does not -- some distributions ship an ffmpeg
/// built without it for licensing reasons.
pub fn software_encodes() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .args([
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=0.2",
            "-c:v", "libx264", "-preset", "ultrafast",
            "-f", "null", "-",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Intel Quick Sync, checked the same "prove it by doing it" way as NVENC.
/// Windows-only in practice (ffmpeg's `h264_qsv` needs the Intel Media SDK
/// runtime, which ships with Intel's Windows graphics driver); harmless to
/// leave callable on Linux too since it will just fail the probe there.
pub fn qsv_encodes() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .args([
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=0.2",
            "-c:v", "h264_qsv",
            "-f", "null", "-",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// AMD AMF, checked the same "prove it by doing it" way as NVENC/QSV.
pub fn amf_encodes() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .args([
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=0.2",
            "-c:v", "h264_amf",
            "-f", "null", "-",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Every DRM render node on this machine, sorted for a stable preference
/// order. Linux-only -- `/dev/dri` doesn't exist elsewhere -- so this always
/// returns empty on Windows rather than erroring; callers (`working_vaapi_node`,
/// `--doctor`'s VA-API check) already treat "no nodes" as a normal, expected
/// outcome, not a failure.
pub fn render_nodes() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let mut nodes = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/dev/dri") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("renderD") {
                    nodes.push(format!("/dev/dri/{name}"));
                }
            }
        }
        nodes.sort();
        nodes
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// Whether `node` can really hardware-encode `codec`, established by encoding
/// through it rather than by inspecting anything. Costs a few hundred
/// milliseconds once at startup, which is a fair price for not shipping a
/// stream that silently produces no frames.
pub fn node_can_encode(node: &str, codec: &str) -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-init_hw_device"])
        .arg(format!("vaapi=va:{node}"))
        .args([
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=0.2",
            "-vf", "format=nv12,hwupload",
            "-c:v", codec,
            "-f", "null", "-",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

/// Locates `config/` by walking up from the CWD (developer checkout
/// workflow), falling back to the platform's standard per-user config
/// location for a release install, which has no `config/host.example.toml`
/// anywhere to find -- `scripts/install.sh`/`install.ps1` seed `host.toml`
/// directly into that directory for exactly this case.
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
            break;
        }
    }

    #[cfg(windows)]
    {
        let base = std::env::var("APPDATA").map(PathBuf::from).context(
            "could not locate the config directory -- %APPDATA% is not set; set \
             PALMTOP_CONFIG_DIR explicitly",
        )?;
        return Ok(base.join("palmtop"));
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
            .context(
                "could not locate the config/ directory, and neither XDG_CONFIG_HOME nor HOME \
                 is set to fall back to -- set PALMTOP_CONFIG_DIR explicitly",
            )?;
        Ok(base.join("palmtop"))
    }
}

/// Primary outbound IPv4 address, found by asking the routing table which
/// source address it would use (no packets are actually sent).
///
/// Public because it has to be re-asked periodically, not just at startup:
/// laptops move between networks constantly and the address is only true
/// until they do. See `palmtopd::pairing::watch_address`.
pub fn detect_primary_ip() -> Result<String> {
    use std::net::UdpSocket;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.connect("8.8.8.8:53")?;
    Ok(sock.local_addr()?.ip().to_string())
}

/// 16 random bytes, hex-encoded, read straight from the OS CSPRNG. Avoids
/// pulling in a `rand` dependency for something the OS already provides
/// directly on both platforms Palmtop hosts on.
#[cfg(not(windows))]
fn generate_token() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .context("open /dev/urandom")?
        .read_exact(&mut bytes)
        .context("read /dev/urandom")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Same contract as the Linux version above, via `BCryptGenRandom` -- the
/// Windows CNG API's own CSPRNG, not a userspace fallback. Not yet exercised
/// by this machine (no Windows target/compiler available here); correctness
/// verified against the `windows` crate's published signature
/// (`Option<BCRYPT_ALG_HANDLE>, &mut [u8], BCRYPTGENRANDOM_FLAGS -> NTSTATUS`)
/// rather than a local compile -- confirm on CI/a real Windows machine before
/// trusting this for anything beyond the pairing token it currently guards.
#[cfg(windows)]
fn generate_token() -> Result<String> {
    use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let mut bytes = [0u8; 16];
    unsafe {
        BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
            .ok()
            .context("BCryptGenRandom")?;
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn every_backend_has_a_distinct_ffmpeg_codec_name() {
        let names = [
            EncodeBackend::Vaapi { render_node: "/dev/dri/renderD128".to_string() }.codec_name(),
            EncodeBackend::Nvenc.codec_name(),
            EncodeBackend::Qsv.codec_name(),
            EncodeBackend::Amf.codec_name(),
            EncodeBackend::Software.codec_name(),
        ];
        let mut unique = names.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "two backends share a codec_name: {names:?}");
    }

    #[test]
    fn qsv_and_amf_display_as_the_vendor_name_not_the_ffmpeg_codec() {
        // A raw "h264_qsv"/"h264_amf" in an error message means nothing to
        // someone who doesn't already know ffmpeg's encoder names -- the
        // whole point of Display existing separately from codec_name.
        assert_eq!(EncodeBackend::Qsv.to_string(), "Intel Quick Sync (QSV)");
        assert_eq!(EncodeBackend::Amf.to_string(), "AMD AMF");
    }

    // HostConfig::load() resolves its directory through the PALMTOP_CONFIG_DIR
    // env var, which is process-wide -- serialize the tests that set it so
    // they can't race each other under cargo test's default parallelism.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn unique_temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "palmtop-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Mirrors config/host.example.toml's real shape: [encode] is the last
    // real table, followed only by a comment that happens to contain the
    // literal substring "[pairing]" despite there being no real [pairing]
    // table anywhere in the file.
    const HOST_TOML_WITH_MISLEADING_COMMENT: &str = r#"
[host]
ip = ""
port = 9999

[gpu]
vaapi_render_node = "/dev/dri/renderD128"

[encode]
codec = "h264_vaapi"
qp = 24
fps = 30
async_depth = 1

# [pairing] is NOT listed here on purpose -- generated on first run.
"#;

    #[test]
    fn a_fresh_config_survives_repeated_loads_without_crashing_or_duplicating_the_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = unique_temp_dir();
        std::fs::write(dir.join("host.toml"), HOST_TOML_WITH_MISLEADING_COMMENT).unwrap();
        std::env::set_var("PALMTOP_CONFIG_DIR", &dir);

        let first = HostConfig::load().expect("first load should generate pairing info");
        assert!(!first.pairing.token.is_empty());
        assert!(!first.pairing.noise_public_key.is_empty());

        // The real bug this reproduces: a second load (e.g. a systemd
        // restart) used to append a *second* generated token, because the
        // first run's token had silently attached to the [encode] table
        // instead of a real [pairing] table and was never read back as
        // `pairing.token`. That produced a duplicate-key TOML parse error
        // on exactly this second load, which crash-looped the daemon for
        // good on a real user's machine.
        let second = HostConfig::load().expect("second load must not crash");
        assert_eq!(first.pairing.token, second.pairing.token);
        assert_eq!(first.pairing.noise_public_key, second.pairing.noise_public_key);

        // A third load, matching the crash-loop restart count actually
        // observed.
        let third = HostConfig::load().expect("third load must not crash either");
        assert_eq!(first.pairing.token, third.pairing.token);

        std::env::remove_var("PALMTOP_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_generated_pairing_section_is_a_real_toml_table_not_just_matching_text() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = unique_temp_dir();
        std::fs::write(dir.join("host.toml"), HOST_TOML_WITH_MISLEADING_COMMENT).unwrap();
        std::env::set_var("PALMTOP_CONFIG_DIR", &dir);

        HostConfig::load().expect("load should succeed");
        let persisted = std::fs::read_to_string(dir.join("host.toml")).unwrap();
        assert!(
            persisted.lines().any(|line| line.trim() == "[pairing]"),
            "expected a real [pairing] section header, got:\n{persisted}"
        );

        std::env::remove_var("PALMTOP_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }
}
