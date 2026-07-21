//! Phase 0 spike: capture real desktop frames (proven in spike-portal-capture)
//! and feed them through hardware VA-API H.264 encode (proven standalone via
//! an `ffmpeg` CLI smoke test) -- this spike is the two joined together, on
//! real desktop content, with real throughput numbers.
//!
//! Requires clicking the portal's "Share Screen" consent dialog, same as
//! spike-portal-capture.
//!
//! Run:  cargo run -p spike-capture-encode
//! Captures ~60 real frames, hardware-encodes them via VA-API on the render
//! node from `config/host.toml`, reports input/output size and throughput,
//! then deletes the raw+encoded scratch files (they contain real desktop
//! content).

use std::cell::RefCell;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use ashpd::WindowIdentifier;
use pipewire as pw;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::pod::{serialize::PodSerializer, Object, Pod, Property, Value};
use pw::spa::utils::{Direction, Id, SpaTypes};
use pw::stream::StreamFlags;

const TARGET_FRAMES: u32 = 60;

#[derive(Default)]
struct CaptureStats {
    width: usize,
    height: usize,
    format: Option<VideoFormat>,
    frames: u32,
}

struct UserData {
    format: VideoInfoRaw,
    have_format: bool,
    raw_writer: BufWriter<std::fs::File>,
    stats: Rc<RefCell<CaptureStats>>,
    mainloop: pw::main_loop::MainLoopRc,
}

fn main() -> Result<()> {
    // GPU/encoder settings are machine-specific -- see config/README.md.
    let host_cfg = palmtop_config::HostConfig::load()?;
    println!(
        "[cfg] vaapi node {} | {} qp{}",
        host_cfg.gpu.vaapi_render_node, host_cfg.encode.codec, host_cfg.encode.qp
    );

    let rt = tokio::runtime::Runtime::new()?;
    let (node_id, fd) = rt.block_on(request_screencast())?;
    println!("[ok] portal approved: pipewire node_id={node_id}");

    let raw_path = std::env::temp_dir().join("palmtop-capture-encode.rawvideo");
    let stats = std::thread::spawn({
        let raw_path = raw_path.clone();
        move || run_capture(fd, node_id, raw_path)
    })
    .join()
    .expect("capture thread panicked")?;

    let raw_bytes = std::fs::metadata(&raw_path).map(|m| m.len()).unwrap_or(0);
    println!(
        "[ok] captured {} real frames, {}x{} {:?}, {} MB raw",
        stats.frames,
        stats.width,
        stats.height,
        stats.format,
        raw_bytes / 1_000_000
    );

    let encode_result = encode_with_vaapi(&raw_path, stats.width, stats.height, &host_cfg);

    std::fs::remove_file(&raw_path).ok();
    println!("[ok] scratch files removed (contained real desktop content)");
    encode_result
}

async fn request_screencast() -> Result<(u32, std::os::fd::OwnedFd)> {
    let proxy = Screencast::new().await.context("connect to portal")?;
    let session = proxy.create_session().await.context("create session")?;

    proxy
        .select_sources(
            &session,
            CursorMode::Embedded, // xdg-desktop-portal-hyprland doesn't support Metadata
            SourceType::Monitor.into(),
            false,
            None,
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

    let fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .context("open_pipe_wire_remote")?;

    Ok((node_id, fd))
}

fn run_capture(fd: std::os::fd::OwnedFd, node_id: u32, raw_path: PathBuf) -> Result<CaptureStats> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None).context("create pw main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None).context("create pw context")?;
    let core = context
        .connect_fd_rc(fd, None)
        .context("connect to portal pipewire remote")?;

    let raw_file = std::fs::File::create(&raw_path).context("create raw scratch file")?;
    let stats = Rc::new(RefCell::new(CaptureStats::default()));
    let data = UserData {
        format: VideoInfoRaw::new(),
        have_format: false,
        raw_writer: BufWriter::new(raw_file),
        stats: stats.clone(),
        mainloop: mainloop.clone(),
    };

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };
    let stream = pw::stream::StreamRc::new(core, "palmtop-capture-encode-spike", props)
        .context("create pw stream")?;

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|_, user_data, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            if user_data.format.parse(param).is_ok() {
                user_data.have_format = true;
                let mut stats = user_data.stats.borrow_mut();
                stats.width = user_data.format.size().width as usize;
                stats.height = user_data.format.size().height as usize;
                stats.format = Some(user_data.format.format());
                println!(
                    "[ok] negotiated video format: {:?} {}x{}",
                    stats.format, stats.width, stats.height
                );
            }
        })
        .process(|stream, user_data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if !user_data.have_format {
                return;
            }
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let stride = datas[0].chunk().stride() as usize;
            let (width, height) = {
                let stats = user_data.stats.borrow();
                (stats.width, stats.height)
            };
            let Some(bytes) = datas[0].data() else { return };
            if width == 0 || height == 0 || bytes.len() < stride * height {
                return;
            }

            // Strip any row padding so the dump is tightly packed BGRA,
            // matching what `ffmpeg -f rawvideo` expects for -s WxH.
            for row in 0..height {
                let start = row * stride;
                user_data
                    .raw_writer
                    .write_all(&bytes[start..start + width * 4])
                    .ok();
            }

            let frames = {
                let mut stats = user_data.stats.borrow_mut();
                stats.frames += 1;
                stats.frames
            };
            if frames % 15 == 0 || frames == TARGET_FRAMES {
                println!("[..] captured {frames} frames");
            }
            if frames >= TARGET_FRAMES {
                user_data.raw_writer.flush().ok();
                user_data.mainloop.quit();
            }
        })
        .register()
        .context("register stream listener")?;

    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw() as u32,
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw() as u32,
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

    let ml_for_timer = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        println!("[warn] 20s timeout -- quitting with whatever was captured.");
        ml_for_timer.quit();
    });
    timer
        .update_timer(Some(Duration::from_secs(20)), None)
        .into_result()
        .context("arm safety timer")?;

    mainloop.run();
    drop(_listener);
    drop(stream);

    Ok(Rc::try_unwrap(stats)
        .map(|c| c.into_inner())
        .unwrap_or_default())
}

fn encode_with_vaapi(
    raw_path: &PathBuf,
    width: usize,
    height: usize,
    cfg: &palmtop_config::HostConfig,
) -> Result<()> {
    if width == 0 || height == 0 {
        anyhow::bail!("no negotiated size available for encode step");
    }
    let out_path = std::env::temp_dir().join("palmtop-capture-encode.h264");

    let start = Instant::now();
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "warning", "-init_hw_device"])
        .arg(format!("vaapi=va:{}", cfg.gpu.vaapi_render_node))
        .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-s"])
        .arg(format!("{width}x{height}"))
        .args(["-r", &cfg.encode.fps.to_string(), "-i"])
        .arg(raw_path)
        .args(["-vf", "format=nv12,hwupload", "-c:v", &cfg.encode.codec])
        .args(["-qp", &cfg.encode.qp.to_string(), "-f", "h264"])
        .arg(&out_path)
        .stdin(Stdio::null())
        .status()
        .context("spawn ffmpeg")?;
    let elapsed = start.elapsed();

    if !status.success() {
        anyhow::bail!("ffmpeg VA-API encode failed with {status}");
    }

    let out_size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    let in_size = std::fs::metadata(raw_path).map(|m| m.len()).unwrap_or(0);
    let frames = in_size / (width * height * 4) as u64;
    let fps = frames as f64 / elapsed.as_secs_f64();

    println!(
        "[ok] VA-API encoded {} real frames ({} MB raw -> {} KB H.264) in {:.2}s ({:.0} fps, {:.1}x realtime @30fps target)",
        frames,
        in_size / 1_000_000,
        out_size / 1_000,
        elapsed.as_secs_f64(),
        fps,
        fps / 30.0,
    );

    std::fs::remove_file(&out_path).ok();
    Ok(())
}
