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

        // An explicit pin is tried first, and *every* backend is pinnable the
        // same way -- including VA-API, which used to be impossible to pin
        // because "h264_vaapi" was overloaded to also mean "figure it out".
        // Auto-detection picks the first backend that works, which is not
        // necessarily the one that feels best: on a hybrid laptop, iGPU
        // VA-API and dGPU NVENC differ in latency, power draw, and quality in
        // ways only the person watching the stream can judge. Hence a real
        // choice -- see `palmtopd --list-encoders`.
        if !configured.is_empty() && configured != AUTO_CODEC {
            if let Some(backend) = self.pinned_backend(configured) {
                return Ok(backend);
            }
            // Warn and fall through rather than bail: a pin that stops working
            // (driver update, GPU swapped out, eGPU unplugged) should degrade
            // to a working stream with a loud explanation, not to no stream at
            // all. This is also what makes changing the template default to
            // "auto" safe for every host.toml already in the wild carrying the
            // old "h264_vaapi" value.
            eprintln!(
                "[gpu] configured codec {configured} does not work on this machine -- falling \
                 back to auto-detection. Run `palmtopd --list-encoders` to see what does work \
                 here, and `palmtopd --set-encoder <codec>` to pin one of those instead."
            );
        }

        // "auto" (and an empty/unrecognised value) is
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

    /// The backend `codec` names, if it actually works on this machine.
    /// `None` covers both "unknown name" and "known but not working here" --
    /// the caller treats them the same way (warn, fall back to auto), and
    /// distinguishing them would only matter for a typo, which
    /// `--set-encoder` already rejects up front.
    fn pinned_backend(&self, codec: &str) -> Option<EncodeBackend> {
        match codec {
            "h264_vaapi" => working_vaapi_node(&self.gpu.vaapi_render_node, "h264_vaapi")
                .map(|render_node| EncodeBackend::Vaapi { render_node }),
            "h264_nvenc" => nvenc_encodes().then_some(EncodeBackend::Nvenc),
            "h264_qsv" => qsv_encodes().then_some(EncodeBackend::Qsv),
            "h264_amf" => amf_encodes().then_some(EncodeBackend::Amf),
            "libx264" => software_encodes().then_some(EncodeBackend::Software),
            _ => {
                eprintln!(
                    "[gpu] unknown codec {codec:?} in host.toml -- valid values are {}.",
                    selectable_codecs().join(", ")
                );
                None
            }
        }
    }

    /// Probes every backend Palmtop knows, in the order auto-detection would
    /// try them, and reports which actually work **by encoding through each
    /// one** -- the same rule `--doctor` follows, for the same reason: a
    /// menu that offers a backend this machine cannot actually use is worse
    /// than no menu.
    ///
    /// Costs a few hundred milliseconds per backend, which is why it is a
    /// deliberate command (`--list-encoders`) rather than something the
    /// daemon does on every start.
    pub fn probe_backends(&self) -> Vec<BackendProbe> {
        let mut out = Vec::new();

        let vaapi_node = working_vaapi_node(&self.gpu.vaapi_render_node, "h264_vaapi");
        out.push(BackendProbe {
            codec: "h264_vaapi",
            label: match &vaapi_node {
                Some(node) => format!("VA-API on {node}"),
                None => "VA-API (Intel/AMD GPU)".to_string(),
            },
            works: vaapi_node.is_some(),
        });
        out.push(BackendProbe {
            codec: "h264_nvenc",
            label: "NVENC (NVIDIA GPU)".to_string(),
            works: nvenc_encodes(),
        });
        out.push(BackendProbe {
            codec: "h264_qsv",
            label: "Intel Quick Sync (QSV)".to_string(),
            works: qsv_encodes(),
        });
        out.push(BackendProbe {
            codec: "h264_amf",
            label: "AMD AMF".to_string(),
            works: amf_encodes(),
        });
        out.push(BackendProbe {
            codec: "libx264",
            label: "software (libx264) -- works almost anywhere, costs real CPU".to_string(),
            works: software_encodes(),
        });
        out
    }
}

/// One row of `--list-encoders`: a backend, whether this machine can really
/// use it, and how to name it in host.toml.
pub struct BackendProbe {
    /// The exact value to put in `[encode] codec`.
    pub codec: &'static str,
    pub label: String,
    pub works: bool,
}

/// The `[encode] codec` value meaning "try everything, use what works".
pub const AUTO_CODEC: &str = "auto";

/// Every value `[encode] codec` accepts, for validation and help text.
pub fn selectable_codecs() -> Vec<&'static str> {
    vec![AUTO_CODEC, "h264_vaapi", "h264_nvenc", "h264_qsv", "h264_amf", "libx264"]
}

/// Rewrites `[encode] codec` in host.toml, in place, preserving every comment
/// and every other value in the file.
///
/// Deliberately edits the text rather than re-serialising the parsed config:
/// `host.toml` is a file humans read and annotate (the template is most of
/// the way to documentation), and a round-trip through `toml::to_string`
/// would discard all of it. Handles the three shapes a real file can take --
/// an `[encode]` section with a `codec` line, one without, and no `[encode]`
/// section at all -- and matches only a genuine section header, never a
/// mention of `[encode]` inside a comment. That last part is not
/// hypothetical: the same substring-vs-real-header mistake in the `[pairing]`
/// generator once crash-looped the daemon by appending a duplicate key on
/// every restart (see `HostConfig::load`).
pub fn set_configured_codec(codec: &str) -> Result<PathBuf> {
    if !selectable_codecs().contains(&codec) {
        bail!(
            "unknown codec {codec:?} -- valid values are {}",
            selectable_codecs().join(", ")
        );
    }
    let path = config_dir()?.join("host.toml");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    std::fs::write(&path, rewrite_codec(&text, codec))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// The pure half of [`set_configured_codec`], so the file-shape handling is
/// testable without touching a real config.
fn rewrite_codec(text: &str, codec: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let is_header = |l: &str| {
        let t = l.trim();
        t.starts_with('[') && t.ends_with(']')
    };
    let new_line = format!("codec = \"{codec}\"");

    match lines.iter().position(|l| l.trim() == "[encode]") {
        Some(start) => {
            // The section runs until the next header, or to end of file.
            let end = lines
                .iter()
                .enumerate()
                .skip(start + 1)
                .find(|(_, l)| is_header(l))
                .map(|(i, _)| i)
                .unwrap_or(lines.len());
            // `split('=')` on a commented-out `# codec = ...` yields "# codec",
            // which correctly does not match -- so a commented example line is
            // left alone rather than being silently uncommented and rewritten.
            let existing = (start + 1..end)
                .find(|&i| lines[i].split('=').next().map(str::trim) == Some("codec"));
            match existing {
                Some(i) => lines[i] = new_line,
                None => lines.insert(start + 1, new_line),
            }
        }
        None => {
            if !lines.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                lines.push(String::new());
            }
            lines.push("[encode]".to_string());
            lines.push(new_line);
        }
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
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

    // --- rewrite_codec: the three real file shapes, plus the traps ---------

    #[test]
    fn rewriting_replaces_an_existing_codec_line_and_keeps_everything_else() {
        let before = "[host]\nport = 9999\n\n[encode]\n# a comment\ncodec = \"h264_vaapi\"\nfps = 30\n";
        let after = rewrite_codec(before, "h264_nvenc");
        assert!(after.contains("codec = \"h264_nvenc\""));
        assert!(!after.contains("h264_vaapi"));
        // Neighbouring keys, comments, and other sections must survive --
        // this file is documentation as much as configuration.
        assert!(after.contains("# a comment"));
        assert!(after.contains("fps = 30"));
        assert!(after.contains("port = 9999"));
    }

    #[test]
    fn rewriting_inserts_a_codec_line_when_the_encode_section_has_none() {
        let before = "[host]\nport = 9999\n\n[encode]\nfps = 30\n";
        let after = rewrite_codec(before, "libx264");
        assert!(after.contains("codec = \"libx264\""));
        assert!(after.contains("fps = 30"));
        // Inserted *inside* [encode], not after [host] and not at EOF -- a
        // bare key attaches to whichever table is physically open above it,
        // so position is correctness here, not tidiness.
        let enc = after.find("[encode]").unwrap();
        let codec = after.find("codec =").unwrap();
        assert!(codec > enc, "codec line landed outside [encode]:\n{after}");
    }

    #[test]
    fn rewriting_appends_a_whole_section_when_there_is_no_encode_table() {
        let before = "[host]\nport = 9999\n";
        let after = rewrite_codec(before, "h264_qsv");
        assert!(after.contains("[encode]"));
        assert!(after.contains("codec = \"h264_qsv\""));
        assert!(toml::from_str::<toml::Value>(&after).is_ok(), "produced invalid TOML:\n{after}");
    }

    #[test]
    fn a_mention_of_encode_inside_a_comment_is_not_mistaken_for_the_section() {
        // Exactly the class of bug that crash-looped the daemon once already,
        // via the [pairing] generator's naive text.contains() check.
        let before = "[host]\nport = 9999\n# the [encode] section is generated below\n";
        let after = rewrite_codec(before, "h264_amf");
        assert!(toml::from_str::<toml::Value>(&after).is_ok(), "produced invalid TOML:\n{after}");
        let parsed: toml::Value = toml::from_str(&after).unwrap();
        assert_eq!(parsed["encode"]["codec"].as_str(), Some("h264_amf"));
    }

    #[test]
    fn a_commented_out_codec_line_is_left_alone_not_uncommented() {
        let before = "[encode]\n# codec = \"libx264\"\nfps = 30\n";
        let after = rewrite_codec(before, "h264_nvenc");
        assert!(after.contains("# codec = \"libx264\""), "clobbered the commented example:\n{after}");
        let parsed: toml::Value = toml::from_str(&after).unwrap();
        assert_eq!(parsed["encode"]["codec"].as_str(), Some("h264_nvenc"));
    }

    #[test]
    fn the_section_boundary_is_respected_when_encode_is_not_last() {
        // A `codec` key in a *later* section must not be the one rewritten.
        let before = "[encode]\nfps = 30\n\n[other]\ncodec = \"do-not-touch\"\n";
        let after = rewrite_codec(before, "libx264");
        assert!(after.contains("codec = \"do-not-touch\""), "rewrote the wrong section:\n{after}");
        let parsed: toml::Value = toml::from_str(&after).unwrap();
        assert_eq!(parsed["encode"]["codec"].as_str(), Some("libx264"));
        assert_eq!(parsed["other"]["codec"].as_str(), Some("do-not-touch"));
    }

    #[test]
    fn rewriting_the_real_template_round_trips_to_valid_toml() {
        // The shipped template is the file most users will actually have.
        let template = include_str!("../../../config/host.example.toml");
        for codec in selectable_codecs() {
            let after = rewrite_codec(template, codec);
            let parsed: toml::Value =
                toml::from_str(&after).unwrap_or_else(|e| panic!("{codec}: invalid TOML: {e}\n{after}"));
            assert_eq!(parsed["encode"]["codec"].as_str(), Some(codec));
        }
    }

    #[test]
    fn every_selectable_codec_is_a_real_backend_name_or_auto() {
        // Guards the menu against offering a value resolved_encode_backend
        // would then reject as unknown -- which would present as "I picked
        // NVENC and it silently used something else".
        for codec in selectable_codecs() {
            if codec == AUTO_CODEC {
                continue;
            }
            let known = [
                EncodeBackend::Vaapi { render_node: String::new() }.codec_name(),
                EncodeBackend::Nvenc.codec_name(),
                EncodeBackend::Qsv.codec_name(),
                EncodeBackend::Amf.codec_name(),
                EncodeBackend::Software.codec_name(),
            ];
            assert!(known.contains(&codec), "{codec} is offered but is not a real backend");
        }
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
