//! Windows.Graphics.Capture (WGC) screen capture -- the Windows counterpart
//! to `platform::linux::capture`, implementing the same
//! `request_screencast()` / `run(handle, slot, stop)` two-function shape (see
//! `platform::linux::capture::ScreencastHandle`'s doc comment for why that
//! symmetry is deliberate) so `session.rs` needs no `cfg` of its own.
//!
//! # Verification status
//!
//! Like `platform::windows::input`, this has never been compiled: no
//! Windows target is available in the environment that wrote it (see the
//! Windows host-support plan's research notes). The WinRT/D3D11 interop
//! pattern below -- `IGraphicsCaptureItemInterop::CreateForMonitor`,
//! `CreateDirect3D11DeviceFromDXGIDevice`, `Direct3D11CaptureFramePool::
//! CreateFreeThreaded`, `IDirect3DDxgiInterfaceAccess::GetInterface` --
//! reflects the documented API surface and the `windows-rs` maintainers'
//! own example usage (confirmed against Microsoft Learn and a windows-rs
//! GitHub issue showing real `FrameArrived` handler code) rather than a
//! local build. This is the highest-risk file in the whole Windows
//! host-support effort: capture is the one piece Phase 0 (on Linux) proved
//! needed real, live verification before anything downstream could be
//! trusted, and this file has had none yet.
//!
//! # Why there is no user-approval dialog step here, unlike Linux
//!
//! The Linux path's `request_screencast` blocks on a human clicking "Share
//! Screen" in the XDG portal's dialog -- capturing another sandboxed
//! process's screen is exactly the kind of thing that dialog exists to
//! gate. WGC capturing your own interactive session's own monitor has no
//! equivalent consent prompt; Windows instead shows a **live indicator**
//! (a yellow border around the captured monitor by default) for as long as
//! capture is active, which is suppressed below via `IsBorderRequired` on
//! Windows versions that support it. `session.rs`'s "waiting for you to
//! approve a dialog" status wording is technically Linux-specific, but
//! harmless if seen briefly on Windows -- it is immediately superseded by
//! the "capturing WxH" status once `request_screencast` returns, which
//! here happens almost immediately rather than after a human clicks
//! anything.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use windows::core::Interface;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, HDC, HMONITOR,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

use crate::capture::{monotonic_us, Frame, FrameSlot, PixelFormat};

/// See `platform::linux::capture::ScreencastHandle` -- same role, opaque to
/// `session.rs`, shaped around whatever this platform actually needs to
/// carry between the two calls. Holds the D3D11 device (needed to build the
/// frame pool in `run`) and the capture item (the monitor to capture).
pub struct ScreencastHandle {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    item: GraphicsCaptureItem,
}

/// Finds the primary monitor's `HMONITOR` and wraps it as a
/// `GraphicsCaptureItem`, then builds the D3D11 device `run` will need. No
/// `.await` actually suspends anything here -- kept `async` purely so this
/// matches `platform::linux::capture::request_screencast`'s signature and
/// `session.rs`'s single `rt.block_on(...)` call site needs no `cfg`.
pub async fn request_screencast() -> Result<(ScreencastHandle, (u32, u32))> {
    let monitor = primary_monitor().context("find primary monitor")?;

    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .context("get IGraphicsCaptureItemInterop factory")?;
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForMonitor(monitor) }.context("CreateForMonitor")?;
    let size = item.Size().context("GraphicsCaptureItem::Size")?;

    let (device, context) = create_d3d11_device().context("create D3D11 device")?;

    Ok((ScreencastHandle { device, context, item }, (size.Width as u32, size.Height as u32)))
}

/// Blocking; owns the frame pool and capture session for as long as `stop`
/// is unset, publishing every arrived frame to `slot`. Intended to be
/// called from its own OS thread, mirroring the Linux backend's `run`.
///
/// Unlike PipeWire's `mainloop.run()`, WGC delivers frames on the frame
/// pool's *own* internal worker thread once created via
/// `CreateFreeThreaded` -- this function's own thread does no frame
/// handling itself, it just keeps the session/frame-pool/device alive and
/// polls `stop`, closing everything once set so the caller's `.join()`
/// only returns after teardown genuinely completes (matching the Linux
/// backend's same guarantee, which `session.rs` relies on before ending a
/// session).
pub fn run(handle: ScreencastHandle, slot: Arc<FrameSlot>, stop: Arc<AtomicBool>) -> Result<()> {
    let ScreencastHandle { device, context, item } = handle;

    let dxgi_device: IDXGIDevice = device.cast().context("ID3D11Device as IDXGIDevice")?;
    // Returns an IInspectable; CreateFreeThreaded below wants the concrete
    // IDirect3DDevice (the WinRT projection of the same underlying device), so
    // cast across -- both are the same COM object, this only re-types the
    // handle.
    let d3d_device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
        .context("CreateDirect3D11DeviceFromDXGIDevice")?
        .cast()
        .context("IInspectable as IDirect3DDevice")?;

    let size = item.Size().context("GraphicsCaptureItem::Size")?;
    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &d3d_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1, // one buffer: "never queue, drop stale" applies here exactly as
           // it does to FrameSlot itself -- see capture.rs's module doc.
        size,
    )
    .context("Direct3D11CaptureFramePool::CreateFreeThreaded")?;

    let handler_slot = slot.clone();
    let handler_device = device.clone();
    let handler_context = context.clone();
    // Signature confirmed against real windows-rs usage (a FrameArrived
    // handler registered via TypedEventHandler::<Direct3D11CaptureFramePool,
    // IInspectable>::new(move |frame_pool, _| { .. Ok(()) })) rather than
    // guessed -- see this file's module doc comment for the source. What is
    // *not* independently confirmed is TryGetNextFrame's exact return shape
    // (whether "no frame available" surfaces as an error or an empty/null
    // frame object); this loop assumes the former, matching how most
    // Result-returning WinRT projections in `windows-rs` behave.
    let handler = TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(
        move |frame_pool, _| {
            let Some(pool) = frame_pool else { return Ok(()) };
            // Exactly one frame per event, not a drain loop. FrameArrived
            // fires once per captured frame, so one-per-event is both correct
            // and the documented pattern -- whereas looping until
            // TryGetNextFrame stops returning Ok assumes a failure mode that
            // is not guaranteed to occur, and would spin the frame pool's own
            // worker thread if it never did. The frame pool is created with a
            // single buffer anyway (see below), so there is never a backlog
            // here to drain: that is the "never queue, drop stale" invariant
            // doing its job one stage earlier.
            if let Ok(frame) = pool.TryGetNextFrame() {
                if let Err(e) = publish_frame(&frame, &handler_device, &handler_context, &handler_slot) {
                    eprintln!("[capture] dropped a frame: {e:#}");
                }
            }
            Ok(())
        },
    );
    frame_pool.FrameArrived(&handler).context("register FrameArrived")?;

    let session: GraphicsCaptureSession =
        frame_pool.CreateCaptureSession(&item).context("CreateCaptureSession")?;
    // Best-effort: only present on Windows 11 21H2+. A capture indicator
    // border is the correct, unsurprising default if this call is
    // unavailable or fails on an older Windows 10 host -- not worth
    // failing capture over.
    let _ = session.SetIsBorderRequired(false);
    session.StartCapture().context("StartCapture")?;

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }

    let _ = session.Close();
    let _ = frame_pool.Close();
    Ok(())
}

/// Copies one arrived frame's GPU texture into system memory and publishes
/// it to `slot`, stripping row-pitch padding exactly like the Linux
/// backend strips PipeWire's stride padding in its `process` callback --
/// same reason: `Frame::bytes` is documented as tightly packed, and
/// `encode::run_feeder` pipes it straight into ffmpeg with no per-row
/// awareness of its own.
fn publish_frame(
    frame: &windows::Graphics::Capture::Direct3D11CaptureFrame,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    slot: &FrameSlot,
) -> Result<()> {
    let surface = frame.Surface().context("Direct3D11CaptureFrame::Surface")?;
    let access: IDirect3DDxgiInterfaceAccess =
        surface.cast().context("IDirect3DSurface as IDirect3DDxgiInterfaceAccess")?;
    let source: ID3D11Texture2D =
        unsafe { access.GetInterface() }.context("GetInterface -> ID3D11Texture2D")?;

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { source.GetDesc(&mut desc) };

    // A CPU-readable staging copy: the captured texture itself lives in
    // GPU-only memory and cannot be Map()'d directly.
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        BindFlags: 0,
        MiscFlags: 0,
        ..desc
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
        .context("CreateTexture2D (staging)")?;
    let staging = staging.context("CreateTexture2D returned no texture")?;

    unsafe { context.CopyResource(&staging, &source) };

    let mut mapped = Default::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.context("Map staging texture")?;

    let width = desc.Width as usize;
    let height = desc.Height as usize;
    let stride = mapped.RowPitch as usize;
    let mut packed = Vec::with_capacity(width * height * 4);
    unsafe {
        let base = mapped.pData as *const u8;
        for row in 0..height {
            let row_start = base.add(row * stride);
            packed.extend_from_slice(std::slice::from_raw_parts(row_start, width * 4));
        }
        context.Unmap(&staging, 0);
    }

    slot.publish(Frame {
        width: width as u32,
        height: height as u32,
        // WGC was created with DirectXPixelFormat::B8G8R8A8UIntNormalized
        // above, so this is BGRA by construction, not by inspection --
        // unlike the Linux backend, which negotiates whatever the
        // compositor offers and has to check.
        format: PixelFormat::Bgra,
        bytes: packed,
        capture_us: monotonic_us(),
    });
    Ok(())
}

/// The primary monitor's `HMONITOR`, via `EnumDisplayMonitors` -- the first
/// (and, for the common single/extended-desktop case, only) monitor
/// enumerated whose origin is `(0, 0)`, which is how Windows identifies the
/// primary display.
fn primary_monitor() -> Result<HMONITOR> {
    let mut found: Option<HMONITOR> = None;
    unsafe {
        let found_ptr = &mut found as *mut Option<HMONITOR> as isize;
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_enum_proc),
            windows::Win32::Foundation::LPARAM(found_ptr),
        );
    }
    found.context("EnumDisplayMonitors found no primary monitor")
}

unsafe extern "system" fn monitor_enum_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    rect: *mut windows::Win32::Foundation::RECT,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::BOOL {
    let is_primary = rect.as_ref().is_some_and(|r| r.left == 0 && r.top == 0);
    if is_primary {
        let out = lparam.0 as *mut Option<HMONITOR>;
        *out = Some(monitor);
        return windows::Win32::Foundation::BOOL(0); // stop enumerating
    }
    windows::Win32::Foundation::BOOL(1) // keep looking
}

/// A hardware D3D11 device with `BGRA_SUPPORT` (required for WGC's
/// `B8G8R8A8UIntNormalized` frame pool format) and its immediate context.
fn create_d3d11_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .context("D3D11CreateDevice")?;
    Ok((
        device.context("D3D11CreateDevice returned no device")?,
        context.context("D3D11CreateDevice returned no context")?,
    ))
}
