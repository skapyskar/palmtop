//! Continuous portal + PipeWire screen capture -- the Linux capture backend.
//!
//! This is the Phase 0 capture spike (`spike-portal-capture`) generalised to
//! run forever instead of stopping after a fixed frame count, and to publish
//! into a single-slot [`FrameSlot`] instead of a file. The single-slot design
//! is deliberate: it is the same "never queue, drop stale work" principle the
//! decode spike proved was load-bearing (README §"Proven: MediaCodec hardware
//! decode"), applied one stage earlier. A slow encoder should skip frames, not
//! build a backlog.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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

use crate::capture::{monotonic_us, Frame, FrameSlot, PixelFormat};

/// Walks the portal ScreenCast flow. Async because `ashpd` is; the result
/// (node id + fd) is handed to [`run`], which is synchronous and blocking.
/// The advertised `(width, height)` lets the caller configure the encoder
/// immediately, without waiting for PipeWire's own format-negotiation
/// callback (which fires slightly later, inside the capture loop).
pub async fn request_screencast() -> Result<(u32, std::os::fd::OwnedFd, (u32, u32))> {
    let proxy = Screencast::new().await.context("connect to portal")?;
    let session = proxy.create_session().await.context("create session")?;
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded, // xdg-desktop-portal-hyprland doesn't support Metadata
            SourceType::Monitor.into(),
            false,
            None,
            PersistMode::DoNot, // TODO(#7 mDNS/pairing task): use restore_token for silent reconnect
        )
        .await
        .context("select_sources")?;
    println!("[capture] waiting for the screen-share dialog to be approved...");
    let response = proxy
        .start(&session, &WindowIdentifier::default())
        .await
        .context("start")?
        .response()
        .context("start response")?;
    let pw_stream = response.streams().first().context("portal returned no streams")?;
    let node_id = pw_stream.pipe_wire_node_id();
    let (w, h) = pw_stream
        .size()
        .context("portal stream did not advertise a size")?;
    let fd = proxy.open_pipe_wire_remote(&session).await.context("open_pipe_wire_remote")?;
    Ok((node_id, fd, (w as u32, h as u32)))
}

/// Blocking; runs the PipeWire main loop until `stop` is set. Intended to be
/// called from its own OS thread.
pub fn run(
    fd: std::os::fd::OwnedFd,
    node_id: u32,
    slot: Arc<FrameSlot>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("pw main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None).context("pw context")?;
    let core = context.connect_fd_rc(fd, None).context("connect portal pipewire remote")?;

    let data = StreamData { format: VideoInfoRaw::new(), have_format: false, slot };
    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };
    let stream =
        pw::stream::StreamRc::new(core, "palmtopd-capture", props).context("create stream")?;

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|_, d, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            if d.format.parse(param).is_ok() {
                d.have_format = true;
                println!(
                    "[capture] format {:?} {}x{}",
                    d.format.format(),
                    d.format.size().width,
                    d.format.size().height
                );
            }
        })
        .process(|stream, d| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            if !d.have_format {
                return;
            }
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let stride = datas[0].chunk().stride() as usize;
            let width = d.format.size().width as usize;
            let height = d.format.size().height as usize;
            let Some(bytes) = datas[0].data() else { return };
            if width == 0 || height == 0 || stride == 0 || bytes.len() < stride * height {
                return;
            }

            let mut packed = Vec::with_capacity(width * height * 4);
            for row in 0..height {
                let start = row * stride;
                packed.extend_from_slice(&bytes[start..start + width * 4]);
            }
            let format = if d.format.format() == VideoFormat::BGRA {
                PixelFormat::Bgra
            } else {
                PixelFormat::Other(format!("{:?}", d.format.format()))
            };
            d.slot.publish(Frame {
                width: width as u32,
                height: height as u32,
                format,
                bytes: packed,
                capture_us: monotonic_us(),
            });
        })
        .register()
        .context("register listener")?;

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

    // Poll the stop flag periodically -- MainLoopRc isn't Send, so the flag
    // can't be waited on from another thread; it has to be checked from here.
    let ml = mainloop.clone();
    let poll_stop = stop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if poll_stop.load(Ordering::Relaxed) {
            ml.quit();
        }
    });
    timer
        .update_timer(Some(Duration::from_millis(200)), Some(Duration::from_millis(200)))
        .into_result()
        .context("arm stop-poll timer")?;

    mainloop.run();
    Ok(())
}

struct StreamData {
    format: VideoInfoRaw,
    have_format: bool,
    slot: Arc<FrameSlot>,
}
