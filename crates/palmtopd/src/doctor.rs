//! Preflight diagnostics: `palmtopd --doctor`.
//!
//! Every check here exists because something in this pipeline can fail in a
//! way that is *invisible from the phone*. The daemon starts, the QR renders,
//! the phone connects -- and then nothing appears on screen, with no error at
//! either end. Two real reports drove this:
//!
//!   - "the screen sharing prompt is not coming": the portal prompt is only
//!     requested when a client actually connects (see session::handle_client),
//!     so a phone that never completes its handshake looks identical to a
//!     broken portal. Nothing distinguished the two.
//!   - "we got the screen choosing popup but the screen never showed": the
//!     config template hardcodes `/dev/dri/renderD128`, which is simply the
//!     wrong node on plenty of machines (hybrid graphics enumerate several,
//!     and the ordering is not stable across hardware). ffmpeg then fails to
//!     initialise VA-API and exits, the feeder's pipe breaks, and the phone
//!     waits forever for a frame that will never be encoded.
//!
//! The rule this module follows: **never report a capability as working
//! unless it was actually exercised.** Checking that a file exists is not
//! evidence that hardware encode works through it, so `check_vaapi` really
//! runs ffmpeg and really encodes frames.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::Result;

/// One diagnostic line. `fix` is only shown on failure, so it can be
/// specific and long without cluttering a healthy report.
struct Check {
    name: String,
    status: Status,
    detail: String,
    fix: Option<String>,
}

#[derive(PartialEq)]
enum Status {
    Pass,
    Warn,
    Fail,
}

impl Status {
    fn marker(&self) -> &'static str {
        match self {
            Status::Pass => "  ok  ",
            Status::Warn => " warn ",
            Status::Fail => " FAIL ",
        }
    }
}

struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn new() -> Self {
        Self { checks: Vec::new() }
    }

    fn add(&mut self, name: &str, status: Status, detail: impl Into<String>, fix: Option<&str>) {
        self.checks.push(Check {
            name: name.to_string(),
            status,
            detail: detail.into(),
            fix: fix.map(|s| s.to_string()),
        });
    }

    fn pass(&mut self, name: &str, detail: impl Into<String>) {
        self.add(name, Status::Pass, detail, None);
    }

    fn warn(&mut self, name: &str, detail: impl Into<String>, fix: &str) {
        self.add(name, Status::Warn, detail, Some(fix));
    }

    fn fail(&mut self, name: &str, detail: impl Into<String>, fix: &str) {
        self.add(name, Status::Fail, detail, Some(fix));
    }

    fn failures(&self) -> usize {
        self.checks.iter().filter(|c| c.status == Status::Fail).count()
    }

    fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\nPalmtop host diagnostics\n");
        for c in &self.checks {
            let _ = writeln!(out, "[{}] {:<22} {}", c.status.marker(), c.name, c.detail);
        }
        let problems: Vec<&Check> =
            self.checks.iter().filter(|c| c.status != Status::Pass).collect();
        if problems.is_empty() {
            let _ = writeln!(
                out,
                "\nEverything checks out. If the phone still shows nothing, the problem is \
                 between the two devices rather than on this machine -- run the daemon in the \
                 foreground (`palmtopd`) and watch its output while the phone connects."
            );
        } else {
            let _ = writeln!(out, "\nWhat to do:\n");
            for c in problems {
                if let Some(fix) = &c.fix {
                    let _ = writeln!(out, "  {}:\n    {}\n", c.name, fix.replace('\n', "\n    "));
                }
            }
        }
        out
    }
}

/// Runs every check and prints the report. Returns a non-zero-worthy bool so
/// the caller can set an exit code -- scripts should be able to gate on this.
pub fn run(cfg: Option<&palmtop_config::HostConfig>) -> Result<bool> {
    let mut r = Report::new();

    check_session(&mut r);
    check_portal(&mut r);
    check_pipewire(&mut r);
    check_ffmpeg(&mut r);
    check_vaapi(&mut r, cfg);
    check_config(&mut r, cfg);

    print!("{}", r.render());
    Ok(r.failures() == 0)
}

fn env_present(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

fn check_session(r: &mut Report) {
    match env_present("WAYLAND_DISPLAY") {
        Some(v) => r.pass("Wayland session", format!("WAYLAND_DISPLAY={v}")),
        None => r.fail(
            "Wayland session",
            "WAYLAND_DISPLAY is not set",
            "Palmtop captures through the Wayland desktop portal and injects input through\n\
             Wayland's virtual-input protocols. An X11 session supports neither, so capture\n\
             cannot work here at all. Log into a Wayland session and try again.\n\
             If you ARE on Wayland, this usually means the daemon is running somewhere that\n\
             did not inherit your session's environment -- see the systemd note below.",
        ),
    }

    // These are the two the systemd --user service most often lacks. Without
    // them the portal call fails with a DBus error that reads like a portal
    // bug rather than a missing environment, which is why it is checked
    // separately and named plainly.
    for var in ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"] {
        match env_present(var) {
            Some(v) => r.pass(var, v),
            None => r.fail(
                var,
                "not set",
                &format!(
                    "The portal is reached over the session DBus, which cannot be found without\n\
                     ${var}. If you are running under systemd, import it once:\n\
                       systemctl --user import-environment {var}\n\
                       systemctl --user restart palmtopd"
                ),
            ),
        }
    }
}

/// Asks DBus whether the ScreenCast portal interface actually exists.
///
/// A desktop can have `xdg-desktop-portal` installed and running while having
/// no *backend* for the running compositor, in which case the ScreenCast
/// interface is simply absent -- and the symptom is exactly the reported one:
/// the share dialog never appears. Checking the interface (rather than the
/// process) is what distinguishes those two cases.
fn check_portal(r: &mut Report) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            r.warn("Desktop portal", format!("could not probe: {e}"), "This is a bug in the check itself, not your system.");
            return;
        }
    };

    let probe = rt.block_on(async {
        let proxy = ashpd::desktop::screencast::Screencast::new().await?;
        let versions = proxy.available_cursor_modes().await?;
        Ok::<_, ashpd::Error>(versions)
    });

    match probe {
        Ok(modes) => r.pass(
            "Desktop portal",
            format!("ScreenCast available (cursor modes: {modes:?})"),
        ),
        Err(e) => r.fail(
            "Desktop portal",
            format!("ScreenCast unavailable: {e}"),
            "The screen-share dialog comes from xdg-desktop-portal, and it needs a *backend*\n\
             matching your desktop. Install the one for your compositor:\n\
               GNOME             xdg-desktop-portal-gnome\n\
               KDE Plasma        xdg-desktop-portal-kde\n\
               Hyprland          xdg-desktop-portal-hyprland\n\
               Sway/wlroots      xdg-desktop-portal-wlr\n\
             Then log out and back in (the portal starts with your session).",
        ),
    }
}

fn check_pipewire(r: &mut Report) {
    let sock = env_present("XDG_RUNTIME_DIR")
        .map(|d| Path::new(&d).join("pipewire-0"))
        .filter(|p| p.exists());
    match sock {
        Some(p) => r.pass("PipeWire", format!("socket at {}", p.display())),
        None => r.fail(
            "PipeWire",
            "no pipewire-0 socket found",
            "Captured frames are delivered over PipeWire, so it must be running:\n\
               systemctl --user status pipewire\n\
               systemctl --user enable --now pipewire pipewire-pulse",
        ),
    }
}

fn check_ffmpeg(r: &mut Report) {
    match Command::new("ffmpeg").args(["-hide_banner", "-version"]).output() {
        Ok(out) if out.status.success() => {
            let first = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("ffmpeg")
                .to_string();
            r.pass("ffmpeg", first);
        }
        _ => r.fail(
            "ffmpeg",
            "not found on PATH",
            "Hardware encoding runs through ffmpeg:\n\
               Arch      sudo pacman -S ffmpeg\n\
               Fedora    sudo dnf install ffmpeg\n\
               Debian    sudo apt install ffmpeg",
        ),
    }
}

/// The check that matters most, and the one that has to be done by *doing*.
///
/// Enumerates every DRM render node and actually encodes a few frames through
/// each with the configured codec. A node existing proves nothing: hybrid-GPU
/// laptops expose several, typically only one of which can hardware-encode
/// H.264, and the default `/dev/dri/renderD128` is frequently the wrong one.
fn check_vaapi(r: &mut Report, cfg: Option<&palmtop_config::HostConfig>) {
    let codec = cfg.map(|c| c.encode.codec.clone()).unwrap_or_else(|| "h264_vaapi".to_string());
    let configured = cfg.map(|c| c.gpu.vaapi_render_node.clone()).unwrap_or_default();

    let mut nodes: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev/dri") {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("renderD") {
                nodes.push(format!("/dev/dri/{name}"));
            }
        }
    }
    nodes.sort();

    if nodes.is_empty() {
        r.fail(
            "GPU render nodes",
            "none found under /dev/dri",
            "No DRM render node means no hardware encoder. If this is a VM or a headless\n\
             machine, hardware encode is not available at all. On real hardware, check that\n\
             your GPU driver is loaded and that you are in the `video`/`render` group:\n\
               sudo usermod -aG video,render $USER    (then log out and back in)",
        );
        return;
    }

    let working: Vec<&String> = nodes.iter().filter(|n| vaapi_encodes(n, &codec)).collect();

    if working.is_empty() {
        r.fail(
            "VA-API hardware encode",
            format!("no node among [{}] can encode {codec}", nodes.join(", ")),
            "Every render node was tried and none could hardware-encode. Install your\n\
             driver's VA-API package and verify with `vainfo`:\n\
               Intel     intel-media-driver (or libva-intel-driver on older chips)\n\
               AMD       libva-mesa-driver / mesa-va-drivers\n\
               NVIDIA    nvidia-vaapi-driver (or switch the codec to a NVENC encoder)\n\
             If `vainfo` shows no H264 entrypoint, this machine cannot hardware-encode and\n\
             Palmtop will not work on it as currently configured.",
        );
        return;
    }

    let list: Vec<String> = working.iter().map(|s| s.to_string()).collect();
    r.pass("VA-API hardware encode", format!("{codec} works on {}", list.join(", ")));

    // The configured node is checked separately from "some node works",
    // because the failure everyone actually hits is a working GPU behind a
    // wrongly-configured path -- which looks identical to a broken GPU from
    // the phone's side.
    if configured.is_empty() {
        r.warn(
            "Configured render node",
            "not set (will auto-detect at startup)",
            &format!("Nothing to fix -- the daemon will pick {} automatically.", list[0]),
        );
    } else if working.iter().any(|w| **w == configured) {
        r.pass("Configured render node", configured);
    } else {
        r.fail(
            "Configured render node",
            format!("{configured} cannot encode {codec}"),
            &format!(
                "This is almost certainly why the phone connects but never shows a picture:\n\
                 the encoder fails to start and no frame is ever produced.\n\
                 Edit the `vaapi_render_node` line in your host.toml to:\n\
                   vaapi_render_node = \"{}\"\n\
                 or clear it (vaapi_render_node = \"\") to auto-detect on every start.",
                list[0]
            ),
        );
    }
}

/// Encodes a few generated frames and reports whether ffmpeg succeeded.
///
/// Deliberately end-to-end through the same encoder the daemon uses, so a
/// pass here means the real pipeline's encode stage will start. Output goes
/// to /dev/null; only the exit status is of interest.
fn vaapi_encodes(node: &str, codec: &str) -> bool {
    Command::new("ffmpeg")
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

fn check_config(r: &mut Report, cfg: Option<&palmtop_config::HostConfig>) {
    let Some(cfg) = cfg else {
        r.fail(
            "Configuration",
            "could not be loaded",
            "Run ./install.sh to create host.toml, then re-run this check.",
        );
        return;
    };

    if cfg.pairing.token.is_empty() || cfg.pairing.noise_public_key.is_empty() {
        r.fail(
            "Pairing credentials",
            "missing token or host key",
            "These are generated the first time the daemon starts. If they are still missing,\n\
             the daemon has never started successfully -- check:\n\
               journalctl --user -u palmtopd -n 50",
        );
    } else {
        r.pass("Pairing credentials", "token and host key present");
    }

    match cfg.resolved_ip() {
        Ok(ip) => r.pass("Host address", format!("{ip}:{}", cfg.host.port)),
        Err(e) => r.fail(
            "Host address",
            format!("{e:#}"),
            "The daemon could not work out which address to advertise. Set it explicitly\n\
             in host.toml:\n  [host]\n  ip = \"192.168.1.42\"",
        ),
    }
}
