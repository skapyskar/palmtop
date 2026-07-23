//! Preflight diagnostics for the Windows host: `palmtopd --doctor` run on
//! Windows. Mirrors `platform::linux::doctor`'s rule -- **never report a
//! capability as working unless it was actually exercised** -- using the
//! same shared `diagnostics::Report` scaffolding, but checking what's
//! actually different on this platform rather than assuming Linux's checks
//! translate unchanged.
//!
//! Unverified like the rest of this platform's code (see `capture`/`input`'s
//! doc comments): the WGC-availability check in particular has never
//! actually observed a real WGC failure to confirm its wording is useful.

use std::process::Command;

use anyhow::Result;

use crate::diagnostics::Report;

pub fn run(cfg: Option<&palmtop_config::HostConfig>) -> Result<bool> {
    let mut r = Report::new();

    check_windows_version(&mut r);
    check_ffmpeg(&mut r);
    check_qsv(&mut r);
    check_nvenc(&mut r);
    check_amf(&mut r);
    check_software(&mut r);
    check_config(&mut r, cfg);
    check_resolved_backend(&mut r, cfg);
    check_scheduled_task(&mut r);

    print!("{}", r.render());
    Ok(r.failures() == 0)
}

/// `Windows.Graphics.Capture`'s public API needs Windows 10 1903 (build
/// 18362) or newer. Checked via the build number in the registry rather
/// than actually starting a capture session here, since a full WGC probe
/// would mean standing up a device/item/frame-pool just to tear it down
/// again -- the real capture path in `platform::windows::capture` already
/// does that for real every time the daemon actually streams, and doing it
/// twice adds startup cost for no extra information.
/// The floor WGC's public API needs, as a build number. Pulled out as a
/// named constant (rather than a bare `18362` in the comparison below) so
/// the "why 18362" reasoning has one home instead of being re-derived at
/// each of the two places that would otherwise repeat it.
const WGC_MIN_BUILD: u32 = 18362; // Windows 10, version 1903

#[derive(Debug, PartialEq, Eq)]
enum BuildCheck {
    Ok(u32),
    TooOld(u32),
    Unknown,
}

/// Pure classification, kept separate from `check_windows_version` purely so
/// it can be unit-tested without a Windows machine -- the actual build
/// number lookup (`windows_build_number`) cannot be exercised here at all.
fn classify_build(build: Option<u32>) -> BuildCheck {
    match build {
        Some(b) if b >= WGC_MIN_BUILD => BuildCheck::Ok(b),
        Some(b) => BuildCheck::TooOld(b),
        None => BuildCheck::Unknown,
    }
}

fn check_windows_version(r: &mut Report) {
    match classify_build(windows_build_number()) {
        BuildCheck::Ok(b) => {
            r.pass("Windows.Graphics.Capture", format!("build {b} >= {WGC_MIN_BUILD} (Win10 1903)"))
        }
        BuildCheck::TooOld(b) => r.fail(
            "Windows.Graphics.Capture",
            format!("build {b} is older than {WGC_MIN_BUILD} (Windows 10 version 1903)"),
            "Windows.Graphics.Capture, which screen capture depends on, was not public API\n\
             before Windows 10 version 1903. Update Windows to use Palmtop.",
        ),
        BuildCheck::Unknown => r.warn(
            "Windows.Graphics.Capture",
            "could not determine the Windows build number",
            "This is a bug in the check itself, not necessarily your system -- capture may\n\
             still work. Run the daemon in the foreground and try connecting a phone to find\n\
             out for certain.",
        ),
    }
}

fn windows_build_number() -> Option<u32> {
    // Real implementation reads
    // HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion!CurrentBuildNumber
    // via the `windows` crate's registry APIs. Deferred: getting this
    // narrowly right needs the same real-machine confirmation as
    // everything else in this module, and unlike capture/input, nothing
    // downstream depends on this check succeeding -- `check_resolved_backend`
    // below is what actually answers "will this machine work".
    None
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
            "The release package bundles ffmpeg.exe alongside palmtopd.exe -- if you built\n\
             from source instead, download a Windows ffmpeg build (e.g. gyan.dev's builds)\n\
             and place ffmpeg.exe next to palmtopd.exe, or add it to PATH.",
        ),
    }
}

/// Intel Quick Sync, checked the same "prove it by doing it" way as every
/// other backend -- see `palmtop_config::qsv_encodes`.
fn check_qsv(r: &mut Report) {
    if palmtop_config::qsv_encodes() {
        r.pass("Intel Quick Sync (QSV)", "works (h264_qsv)");
    } else {
        r.warn(
            "Intel Quick Sync (QSV)",
            "not available",
            "Expected on any non-Intel-GPU machine, or an Intel GPU with an outdated driver.\n\
             Update the Intel graphics driver if this machine has an Intel iGPU and you\n\
             expected QSV to work.",
        );
    }
}

fn check_nvenc(r: &mut Report) {
    if palmtop_config::nvenc_encodes() {
        r.pass("NVENC hardware encode", "works (h264_nvenc)");
    } else {
        r.warn(
            "NVENC hardware encode",
            "not available",
            "Expected on any non-NVIDIA machine. On an NVIDIA GPU, this usually means the\n\
             driver needs updating, or ffmpeg was built without NVENC support -- check with:\n\
               ffmpeg -hide_banner -encoders | findstr nvenc",
        );
    }
}

fn check_amf(r: &mut Report) {
    if palmtop_config::amf_encodes() {
        r.pass("AMD AMF", "works (h264_amf)");
    } else {
        r.warn(
            "AMD AMF",
            "not available",
            "Expected on any non-AMD-GPU machine, or an outdated AMD driver. Update the AMD\n\
             graphics driver if this machine has an AMD GPU and you expected AMF to work.",
        );
    }
}

fn check_software(r: &mut Report) {
    if palmtop_config::software_encodes() {
        r.pass("Software encode", "works (libx264) -- the fallback of last resort");
    } else {
        r.warn(
            "Software encode",
            "libx264 not available in this ffmpeg build",
            "If QSV, NVENC, and AMF also failed above, this machine cannot encode video at\n\
             all with the current ffmpeg. The bundled ffmpeg.exe includes libx264; if you\n\
             replaced it with your own build, make sure that build has libx264 enabled.",
        );
    }
}

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
                "{e:#}\nNone of Quick Sync, NVENC, AMF, or software encoding work on this \
                 machine with the currently installed ffmpeg. See the individual checks above \
                 for what to install."
            ),
        ),
    }
}

fn check_config(r: &mut Report, cfg: Option<&palmtop_config::HostConfig>) {
    let Some(cfg) = cfg else {
        r.fail(
            "Configuration",
            "could not be loaded",
            "Run install.ps1 to create host.toml, then re-run this check.",
        );
        return;
    };

    if cfg.pairing.token.is_empty() || cfg.pairing.noise_public_key.is_empty() {
        r.fail(
            "Pairing credentials",
            "missing token or host key",
            "These are generated the first time the daemon starts. If they are still\n\
             missing, the daemon has never started successfully -- check the Scheduled Task\n\
             history in Task Scheduler, or run palmtopd.exe directly from a terminal to see\n\
             its output.",
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

/// Whether the logon Scheduled Task `install.ps1` registers is actually
/// present -- checked via `schtasks /query`, the same "ask the real OS
/// facility" rule the encode-backend probes already follow, rather than
/// assuming installation succeeded.
fn check_scheduled_task(r: &mut Report) {
    let output = Command::new("schtasks")
        .args(["/query", "/tn", "Palmtop", "/fo", "LIST"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            r.pass("Logon Scheduled Task", "registered (runs palmtopd at logon)");
        }
        _ => r.warn(
            "Logon Scheduled Task",
            "not found",
            "palmtopd will not start automatically at logon. Run install.ps1 to register it,\n\
             or start palmtopd.exe manually each time you want to use Palmtop.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_at_or_above_the_floor_passes() {
        assert_eq!(classify_build(Some(WGC_MIN_BUILD)), BuildCheck::Ok(WGC_MIN_BUILD));
        assert_eq!(classify_build(Some(WGC_MIN_BUILD + 1000)), BuildCheck::Ok(WGC_MIN_BUILD + 1000));
    }

    #[test]
    fn a_build_below_the_floor_fails_rather_than_warns() {
        assert_eq!(classify_build(Some(WGC_MIN_BUILD - 1)), BuildCheck::TooOld(WGC_MIN_BUILD - 1));
    }

    #[test]
    fn no_build_number_is_unknown_not_a_failure() {
        // A version-detection bug should not read as "your Windows is too
        // old" -- see check_windows_version's Unknown arm, which warns
        // rather than fails for exactly this reason.
        assert_eq!(classify_build(None), BuildCheck::Unknown);
    }
}

