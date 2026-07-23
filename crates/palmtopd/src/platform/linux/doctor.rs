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

use std::path::Path;
use std::process::Command;

use anyhow::Result;

use crate::diagnostics::Report;

/// Runs every check and prints the report. Returns a non-zero-worthy bool so
/// the caller can set an exit code -- scripts should be able to gate on this.
pub fn run(cfg: Option<&palmtop_config::HostConfig>) -> Result<bool> {
    let mut r = Report::new();

    check_session(&mut r);
    check_portal(&mut r);
    check_pipewire(&mut r);
    check_ffmpeg(&mut r);
    check_vaapi(&mut r, cfg);
    check_nvenc(&mut r);
    check_software(&mut r);
    check_config(&mut r, cfg);
    check_resolved_backend(&mut r, cfg);

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
/// Tries every encoder Palmtop knows how to use -- VA-API on each DRM render
/// node, NVENC, and software libx264 -- and reports each independently, since
/// any one of them working is enough for the daemon to actually stream (see
/// `HostConfig::resolved_encode_backend`, which tries them in this same
/// order). A machine failing VA-API is not a machine that cannot run
/// Palmtop; the report used to say exactly that, back when VA-API was the
/// only backend that existed.
fn check_vaapi(r: &mut Report, cfg: Option<&palmtop_config::HostConfig>) {
    let codec = "h264_vaapi";
    let configured = cfg.map(|c| c.gpu.vaapi_render_node.clone()).unwrap_or_default();
    let nodes = palmtop_config::render_nodes();

    if nodes.is_empty() {
        r.warn(
            "VA-API render nodes",
            "none found under /dev/dri",
            "No DRM render node means no VA-API hardware encode. If this is a VM or a\n\
             headless machine, that is expected -- NVENC or software encoding (checked\n\
             separately below) may still work. On real hardware with a GPU, check that\n\
             your GPU driver is loaded and that you are in the `video`/`render` group:\n\
               sudo usermod -aG video,render $USER    (then log out and back in)",
        );
        return;
    }

    let working: Vec<&String> =
        nodes.iter().filter(|n| palmtop_config::node_can_encode(n, codec)).collect();

    if working.is_empty() {
        r.warn(
            "VA-API hardware encode",
            format!("no node among [{}] can encode {codec}", nodes.join(", ")),
            "Every render node was tried and none could hardware-encode. This machine may\n\
             still work via NVENC or software encoding (checked separately below). To fix\n\
             VA-API specifically, install your driver's VA-API package and verify with\n\
             `vainfo`:\n\
               Intel     intel-media-driver (or libva-intel-driver on older chips)\n\
               AMD       libva-mesa-driver / mesa-va-drivers\n\
               NVIDIA    nvidia-vaapi-driver (inconsistently packaged -- NVENC below is\n\
                         usually the more reliable path on NVIDIA)",
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
        r.warn(
            "Configured render node",
            format!("{configured} cannot encode {codec}"),
            &format!(
                "Not fatal -- the daemon auto-heals this at startup by picking a working node\n\
                 instead. To silence this warning, edit the `vaapi_render_node` line in your\n\
                 host.toml to:\n\
                   vaapi_render_node = \"{}\"\n\
                 or clear it (vaapi_render_node = \"\") to auto-detect on every start.",
                list[0]
            ),
        );
    }
}

/// NVENC, checked the same "prove it by doing it" way as VA-API. Common on
/// machines where VA-API is the one that does not work: NVIDIA's VA-API shim
/// is inconsistently packaged across distros, while NVENC support usually
/// just needs the proprietary driver already installed for anything else
/// NVIDIA-related to work.
fn check_nvenc(r: &mut Report) {
    if palmtop_config::nvenc_encodes() {
        r.pass("NVENC hardware encode", "works (h264_nvenc)");
    } else {
        r.warn(
            "NVENC hardware encode",
            "not available",
            "Expected on any non-NVIDIA machine. On an NVIDIA GPU, this usually means the\n\
             proprietary driver is not installed, or ffmpeg was built without NVENC support\n\
             -- check with:  ffmpeg -hide_banner -encoders | grep nvenc",
        );
    }
}

/// Software libx264, the last resort: works on essentially any machine, at
/// the cost of real CPU time instead of a GPU's. Still worth verifying
/// rather than assuming -- some distributions ship an ffmpeg built without
/// libx264 for licensing reasons, in which case even this backend is absent.
fn check_software(r: &mut Report) {
    if palmtop_config::software_encodes() {
        r.pass("Software encode", "works (libx264) -- the fallback of last resort");
    } else {
        r.warn(
            "Software encode",
            "libx264 not available in this ffmpeg build",
            "If VA-API and NVENC also failed above, this machine cannot encode video at all\n\
             with the current ffmpeg. Install a build with libx264, e.g. on Debian/Ubuntu\n\
             the distro ffmpeg package normally includes it already; on a from-source build,\n\
             configure with --enable-libx264 --enable-gpl.",
        );
    }
}

/// The verdict that actually matters: which backend, if any, the daemon will
/// really use. Everything above explains *why*; this is the one line that
/// answers "will this machine work at all".
fn check_resolved_backend(r: &mut Report, cfg: Option<&palmtop_config::HostConfig>) {
    let Some(cfg) = cfg else {
        return; // check_config already reports the missing-config failure.
    };
    match cfg.resolved_encode_backend() {
        Ok(backend) => r.pass("Video encoder", format!("will stream via {backend}")),
        Err(e) => r.fail(
            "Video encoder",
            "no working backend found",
            &format!(
                "{e:#}\nNone of VA-API, NVENC, or software encoding work on this machine with \
                 the currently installed ffmpeg. See the individual checks above for what to \
                 install."
            ),
        ),
    }
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
