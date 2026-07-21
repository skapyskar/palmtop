//! Phase 0 gap-closer: measure the **capture** leg of the latency budget, which
//! was previously only estimated at "<=16ms, event-driven".
//!
//! Method: warp the cursor to a known screen position via the wlr virtual
//! pointer (proven in spike-wlr-input), timestamp the injection, then watch
//! incoming portal/PipeWire frames for the pixels in that region to change.
//! The delta is real end-to-end input -> compositor -> capture latency.
//!
//! This closes both Phase 0 gaps at once:
//!   - it measures capture latency against a known stimulus, and
//!   - it proves injected input actually reaches the compositor and shows up in
//!     captured output (the standalone input spike only proved the protocol was
//!     accepted, never that it had a visible effect).
//!
//! Cursor is captured because the portal session requests CursorMode::Embedded.
//!
//! Run:  cargo run --release -p spike-capture-latency
//! Requires approving the "Share Screen" dialog. Keep the pointer still.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// How many inject/observe rounds to run.
const TRIALS: usize = 20;
/// Per-sample intensity delta counted as "this pixel changed".
///
/// Detection counts *changed samples* rather than averaging over the frame: a
/// cursor is only ~24px on a 1080p screen, so its contribution to a whole-frame
/// mean is ~0.03 -- invisible to a mean-based threshold, but unmistakable as a
/// cluster of strongly-changed samples.
const PIXEL_DELTA: i16 = 20;
/// How many changed samples constitute a real change (vs. sensor/encoder noise).
const MIN_CHANGED_SAMPLES: usize = 4;
/// Downsampling step. Must be small enough that a cursor spans several samples.
const STEP: usize = 4;

/// Shared state between the Wayland injector thread and the PipeWire capture thread.
struct Shared {
    /// Instant (as nanos since process start) when motion was injected; 0 = idle.
    injected_at: AtomicI64,
    /// Set by the capture thread once it observes the change.
    observed: AtomicBool,
    /// Measured latency in microseconds for the current trial.
    latency_us: AtomicU64,
    /// Capture thread has a baseline frame and is ready for a trial.
    ready: AtomicBool,
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (node_id, fd) = rt.block_on(request_screencast())?;
    println!("[ok] portal approved: node_id={node_id}");

    let shared = Arc::new(Shared {
        injected_at: AtomicI64::new(0),
        observed: AtomicBool::new(false),
        latency_us: AtomicU64::new(0),
        ready: AtomicBool::new(false),
    });
    let origin = Instant::now();

    // Capture runs its own C event loop, so give it a dedicated thread.
    let capture_shared = shared.clone();
    let capture = std::thread::spawn(move || run_capture(fd, node_id, capture_shared, origin));

    // Injector drives the trials from this thread.
    let results = run_injector(shared.clone(), origin)?;
    shared.ready.store(false, Ordering::SeqCst);
    let _ = capture.join();

    report(&results);
    Ok(())
}

async fn request_screencast() -> Result<(u32, std::os::fd::OwnedFd)> {
    let proxy = Screencast::new().await.context("connect to portal")?;
    let session = proxy.create_session().await.context("create session")?;
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded, // cursor must be in the pixels for this to work
            SourceType::Monitor.into(),
            false,
            None,
            PersistMode::DoNot,
        )
        .await
        .context("select_sources")?;
    println!("[..] approve the screen-share dialog, then keep the mouse still");
    let response = proxy
        .start(&session, &WindowIdentifier::default())
        .await
        .context("start")?
        .response()
        .context("start response")?;
    let stream = response.streams().first().context("no streams")?;
    let node_id = stream.pipe_wire_node_id();
    let fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .context("open_pipe_wire_remote")?;
    Ok((node_id, fd))
}

// ------------------------------------------------------------------ injector

struct WlState;

fn run_injector(shared: Arc<Shared>, origin: Instant) -> Result<Vec<f64>> {
    let conn = Connection::connect_to_env().context("wayland connect")?;
    let (globals, mut queue) = registry_queue_init::<WlState>(&conn).context("registry")?;
    let qh = queue.handle();
    let mut state = WlState;

    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=8, ()).context("no wl_seat")?;
    let vpm: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .context("no zwlr_virtual_pointer_manager_v1")?;
    let pointer = vpm.create_virtual_pointer(Some(&seat), &qh, ());

    // Wait for the capture thread to establish a baseline.
    let wait_start = Instant::now();
    while !shared.ready.load(Ordering::SeqCst) {
        if wait_start.elapsed() > Duration::from_secs(20) {
            anyhow::bail!("capture thread never became ready");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("[ok] capture ready, running {TRIALS} trials");

    let mut results = Vec::new();
    let mut far = false;

    // Absolute positioning in a normalised 1000x1000 space, so the stimulus is
    // deterministic and resolution-independent. Relative motion can silently
    // clamp at a screen edge and produce no visible change at all.
    const EXTENT: u32 = 1000;
    let (near_xy, far_xy) = ((300u32, 300u32), (700u32, 700u32));

    for trial in 1..=TRIALS {
        // Let the screen settle so the baseline is quiet before we perturb it.
        std::thread::sleep(Duration::from_millis(250));
        shared.observed.store(false, Ordering::SeqCst);
        shared.latency_us.store(0, Ordering::SeqCst);

        let (x, y) = if far { near_xy } else { far_xy };
        far = !far;

        // Timestamp as close to the injection as possible.
        let t0 = origin.elapsed().as_nanos() as i64;
        shared.injected_at.store(t0, Ordering::SeqCst);

        let time_ms = origin.elapsed().as_millis() as u32;
        pointer.motion_absolute(time_ms, x, y, EXTENT, EXTENT);
        pointer.frame();
        conn.flush().context("flush injection")?;

        // Wait for the capture thread to spot it.
        let deadline = Instant::now() + Duration::from_millis(1500);
        while !shared.observed.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_micros(200));
        }
        shared.injected_at.store(0, Ordering::SeqCst);

        if shared.observed.load(Ordering::SeqCst) {
            let ms = shared.latency_us.load(Ordering::SeqCst) as f64 / 1000.0;
            results.push(ms);
            println!("[..] trial {trial:2}/{TRIALS}: {ms:.2} ms");
        } else {
            println!("[warn] trial {trial:2}/{TRIALS}: no change detected (skipped)");
        }
        let _ = queue.roundtrip(&mut state);
    }
    Ok(results)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WlState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
macro_rules! inert {
    ($($t:ty),* $(,)?) => {$(
        impl Dispatch<$t, ()> for WlState {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event,
                     _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
inert!(wl_seat::WlSeat, ZwlrVirtualPointerManagerV1, ZwlrVirtualPointerV1);

// ------------------------------------------------------------------- capture

struct CaptureData {
    format: VideoInfoRaw,
    have_format: bool,
    /// Downsampled previous frame, for cheap whole-screen change detection.
    prev: Vec<u8>,
    shared: Arc<Shared>,
    origin: Instant,
}

fn run_capture(
    fd: std::os::fd::OwnedFd,
    node_id: u32,
    shared: Arc<Shared>,
    origin: Instant,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None).context("context")?;
    let core = context.connect_fd_rc(fd, None).context("connect fd")?;

    let data = CaptureData {
        format: VideoInfoRaw::new(),
        have_format: false,
        prev: Vec::new(),
        shared: shared.clone(),
        origin,
    };

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };
    let stream = pw::stream::StreamRc::new(core, "palmtop-capture-latency", props)
        .context("create stream")?;

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
                    "[ok] capture format {:?} {}x{}",
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
            let w = d.format.size().width as usize;
            let h = d.format.size().height as usize;
            let Some(bytes) = datas[0].data() else { return };
            if w == 0 || h == 0 || stride == 0 || bytes.len() < stride * h {
                return;
            }

            // Timestamp immediately on frame arrival, before any processing, so
            // our own downsampling cost isn't charged to the measurement.
            let arrived = d.origin.elapsed().as_nanos() as i64;

            // Downsample so the per-frame comparison stays cheap while still
            // resolving a cursor-sized object.
            let mut small = Vec::with_capacity((w / STEP + 1) * (h / STEP + 1));
            for y in (0..h).step_by(STEP) {
                let row = y * stride;
                for x in (0..w).step_by(STEP) {
                    small.push(bytes[row + x * 4]); // blue channel is enough
                }
            }

            if d.prev.len() == small.len() {
                let injected = d.shared.injected_at.load(Ordering::SeqCst);
                if injected != 0 && !d.shared.observed.load(Ordering::SeqCst) {
                    let changed = d
                        .prev
                        .iter()
                        .zip(small.iter())
                        .filter(|(a, b)| (**a as i16 - **b as i16).abs() > PIXEL_DELTA)
                        .count();
                    if changed >= MIN_CHANGED_SAMPLES {
                        let us = ((arrived - injected) / 1000).max(0) as u64;
                        d.shared.latency_us.store(us, Ordering::SeqCst);
                        d.shared.observed.store(true, Ordering::SeqCst);
                    }
                }
            } else {
                d.shared.ready.store(true, Ordering::SeqCst);
            }
            d.prev = small;

            if !d.shared.ready.load(Ordering::SeqCst) {
                d.shared.ready.store(true, Ordering::SeqCst);
            }
        })
        .register()
        .context("register listener")?;

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
        .context("serialize pod")?
        .0
        .into_inner();
    let mut params = [Pod::from_bytes(&bytes).context("pod")?];
    stream
        .connect(
            Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .context("connect stream")?;

    // Bounded run: the injector finishes well inside this.
    let ml = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| ml.quit());
    timer
        .update_timer(Some(Duration::from_secs(TRIALS as u64 * 2 + 25)), None)
        .into_result()
        .context("arm timer")?;

    mainloop.run();
    Ok(())
}

fn report(results: &[f64]) {
    if results.is_empty() {
        println!("\n[FAIL] no trials produced a measurement");
        return;
    }
    let mut s = results.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    let mean = s.iter().sum::<f64>() / n as f64;
    let pct = |p: f64| s[((n as f64 * p) as usize).min(n - 1)];

    println!("\n===== capture latency (inject -> compositor -> captured frame) =====");
    println!("trials  : {n}/{TRIALS}");
    println!("mean    : {mean:.2} ms");
    println!("min     : {:.2} ms", s[0]);
    println!("p50     : {:.2} ms", pct(0.50));
    println!("p95     : {:.2} ms", pct(0.95));
    println!("max     : {:.2} ms", s[n - 1]);
    println!(
        "\nNote: includes compositor composite + portal/PipeWire delivery, and is\n\
         quantised by the compositor's refresh interval."
    );
}
