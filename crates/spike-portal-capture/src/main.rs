//! Phase 0 spike: prove xdg-desktop-portal ScreenCast -> PipeWire frame capture
//! works on Hyprland (wlroots).
//!
//! This is the second Phase 0 risk item: does the standard portal capture path
//! (used by Sunshine, OBS, etc.) actually deliver readable pixel data through
//! xdg-desktop-portal-hyprland? Proof = a PPM image written to disk that a human
//! can open and visually confirm is the screen.
//!
//! IMPORTANT: this requires clicking through a portal **consent dialog** the
//! first time it runs (share a monitor). That dialog cannot be approved by an
//! agent -- a human must click it.
//!
//! Run:  cargo run -p spike-portal-capture
//! Expect: a "Share Screen" dialog appears -> pick a monitor -> approve.
//!         Then ~30 frames are captured and the first one is saved as
//!         palmtop-capture-spike.ppm in the current directory.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use ashpd::WindowIdentifier;
use pipewire as pw;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::video::VideoInfoRaw;
use pw::spa::pod::{serialize::PodSerializer, Object, Pod, Property, Value};
use pw::spa::utils::{Direction, Id, SpaTypes};
use pw::stream::StreamFlags;

struct UserData {
    format: VideoInfoRaw,
    have_format: bool,
    frames_seen: u32,
    saved_frame: bool,
    mainloop: pw::main_loop::MainLoopRc,
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (node_id, size, fd) = rt.block_on(request_screencast())?;
    println!(
        "[ok] portal approved: pipewire node_id={node_id} advertised_size={size:?}"
    );

    // pw's main loop is a separate C event loop (not tokio) -- run it on a
    // blocking thread so we don't fight the async runtime.
    std::thread::spawn(move || {
        if let Err(e) = run_capture(fd, node_id) {
            eprintln!("[error] capture failed: {e:?}");
        }
    })
    .join()
    .expect("capture thread panicked");

    Ok(())
}

/// Walks the xdg-desktop-portal ScreenCast flow: create session -> select a
/// monitor source -> start (shows the consent dialog) -> open the PipeWire
/// remote fd. Returns the node id to bind to and the fd to hand to PipeWire.
async fn request_screencast() -> Result<(u32, Option<(i32, i32)>, std::os::fd::OwnedFd)> {
    let proxy = Screencast::new().await.context("connect to portal")?;
    let session = proxy.create_session().await.context("create session")?;

    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            false, // single source only, for the spike
            None,  // no restore_token yet -- always prompts (fine for a spike)
            PersistMode::DoNot,
        )
        .await
        .context("select_sources")?;

    println!("[..] waiting for you to approve the screen-share dialog...");
    let response = proxy
        .start(&session, &WindowIdentifier::default())
        .await
        .context("start")?
        .response()
        .context("start response")?;

    let stream = response
        .streams()
        .first()
        .context("portal returned no streams")?;
    let node_id = stream.pipe_wire_node_id();
    let size = stream.size();

    let fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .context("open_pipe_wire_remote")?;

    Ok((node_id, size, fd))
}

/// Connects to the PipeWire remote via the portal-provided fd, binds the
/// negotiated node, and pulls frames until a handful have been received (or
/// a 20s safety timeout fires with no data).
fn run_capture(fd: std::os::fd::OwnedFd, node_id: u32) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None).context("create pw main loop")?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).context("create pw context")?;
    let core = context
        .connect_fd_rc(fd, None)
        .context("connect to portal pipewire remote")?;

    let data = UserData {
        format: VideoInfoRaw::new(),
        have_format: false,
        frames_seen: 0,
        saved_frame: false,
        mainloop: mainloop.clone(),
    };

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };
    let stream = pw::stream::StreamRc::new(core, "palmtop-capture-spike", props)
        .context("create pw stream")?;

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|_, user_data, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            match user_data.format.parse(param) {
                Ok(_) => {
                    user_data.have_format = true;
                    println!(
                        "[ok] negotiated video format: {:?} {}x{}",
                        user_data.format.format(),
                        user_data.format.size().width,
                        user_data.format.size().height
                    );
                }
                Err(e) => eprintln!("[warn] could not parse negotiated format: {e}"),
            }
        })
        .process(|stream, user_data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let chunk_size = datas[0].chunk().size();
            let stride = datas[0].chunk().stride();
            user_data.frames_seen += 1;
            println!(
                "[ok] frame {} received: {} bytes (stride {})",
                user_data.frames_seen, chunk_size, stride
            );

            if !user_data.saved_frame && user_data.have_format {
                if let Some(bytes) = datas[0].data() {
                    let format = user_data.format.format();
                    let (w, h) = (
                        user_data.format.size().width as usize,
                        user_data.format.size().height as usize,
                    );
                    match save_as_ppm(bytes, w, h, stride as usize, format) {
                        Ok(path) => {
                            println!("[ok] saved first frame to {}", path.display());
                            user_data.saved_frame = true;
                        }
                        Err(e) => eprintln!("[warn] could not convert frame to PPM: {e}"),
                    }
                }
            }

            if user_data.frames_seen >= 30 {
                println!("[done] captured 30 frames, stopping.");
                user_data.mainloop.quit();
            }
        })
        .register()
        .context("register stream listener")?;

    // Accept any raw video format/size/framerate the portal source offers --
    // simplest possible negotiation for a feasibility spike.
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw())),
            ),
        ],
    };
    let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
        .context("serialize format pod")?
        .0
        .into_inner();
    let mut params = [Pod::from_bytes(&bytes).context("build format pod")?];

    stream
        .connect(
            Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .context("connect stream to portal node")?;

    // Safety timeout: if nothing arrives (e.g. dialog dismissed), don't hang forever.
    let ml_for_timer = mainloop.clone();
    let timer = mainloop
        .loop_()
        .add_timer(move |_| {
            println!("[warn] 20s timeout with no frames -- quitting.");
            ml_for_timer.quit();
        });
    timer
        .update_timer(Some(Duration::from_secs(20)), None)
        .into_result()
        .context("arm safety timer")?;

    mainloop.run();
    Ok(())
}

fn save_as_ppm(
    bytes: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    format: pw::spa::param::video::VideoFormat,
) -> Result<PathBuf> {
    use pw::spa::param::video::VideoFormat as F;

    // Reorder each pixel's 4 bytes into RGB, dropping the 4th (alpha/pad) byte.
    let reorder: fn(&[u8]) -> [u8; 3] = match format {
        F::BGRx | F::BGRA => |p| [p[2], p[1], p[0]],
        F::RGBx | F::RGBA => |p| [p[0], p[1], p[2]],
        F::xRGB | F::ARGB => |p| [p[1], p[2], p[3]],
        F::xBGR | F::ABGR => |p| [p[3], p[2], p[1]],
        other => anyhow::bail!("unhandled pixel format {other:?} -- raw bytes not decoded"),
    };

    let mut rgb = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        let row_start = row * stride;
        if row_start + width * 4 > bytes.len() {
            anyhow::bail!("frame buffer shorter than expected (short read)");
        }
        let row_bytes = &bytes[row_start..row_start + width * 4];
        for px in row_bytes.chunks_exact(4) {
            rgb.extend_from_slice(&reorder(px));
        }
    }

    let path = PathBuf::from("palmtop-capture-spike.ppm");
    let header = format!("P6\n{width} {height}\n255\n");
    let mut out = Vec::with_capacity(header.len() + rgb.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&rgb);
    std::fs::write(&path, out).context("write ppm file")?;
    Ok(path)
}
