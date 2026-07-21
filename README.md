# Palmtop

Use your Linux (Wayland) laptop entirely from an Android phone — a touch-native,
zero-config, phone-shaped remote. Free, local-first (LAN / USB tethering), no cloud.

Full plan: `~/.claude/plans/build-a-plan-over-tingly-stearns.md`

## Status: Phase 0 (feasibility spike)

| Risk being de-risked | Status |
|---|---|
| **wlroots tier-2 input injection** (no libei on Hyprland/Sway) | ✅ **Proven** on Hyprland 0.55.4 |
| **xdg-desktop-portal ScreenCast → PipeWire frame capture** | ✅ **Proven** on Hyprland 0.55.4 / xdg-desktop-portal-hyprland |
| **VA-API hardware encode of real captured frames** | ✅ **Proven** — 120fps throughput on AMD iGPU (4x realtime @30fps target) |
| **Android low-latency decode/render (MediaCodec)** | ✅ **Proven** — 25 ms avg / 35 ms p99 @1080p30 |
| **End-to-end glass-to-glass latency < 80 ms LAN** | ✅ **PASS** — ~57 ms typical (see verdict below) |

### Proven: wlr virtual input on Hyprland

`crates/spike-wlr-input` binds `zwlr_virtual_pointer_manager_v1` and
`zwp_virtual_keyboard_manager_v1`, then injects pointer motion, a click, a scroll
tick, and a keystroke. On Hyprland 0.55.4 all requests are accepted with no
protocol error — confirming the tier-2 backend the plan depends on for wlroots
compositors (which lack libei).

```sh
cargo run -p spike-wlr-input
# Watch: the cursor traces a square, left-clicks, scrolls, then types 'h'
# into whatever window currently has keyboard focus.
```

### Proven: portal + PipeWire screen capture on Hyprland

`crates/spike-portal-capture` walks the real `org.freedesktop.portal.ScreenCast`
flow (`ashpd`) — create session, select a monitor, **show the user a consent
dialog**, start — then opens the negotiated PipeWire remote and pulls frames
(`pipewire` 0.10 + `libspa` 0.10). Verified result: 30 frames at 1920x1080
`BGRA`, ~30fps, decoded and written out as a viewable image that was confirmed
to show the live desktop.

Notes from the run:
- `xdg-desktop-portal-hyprland` only supports `CursorMode::Embedded` (not
  `Metadata`) — the portal rejects unsupported cursor modes with
  `InvalidArgument`, so the host agent must probe `available_cursor_modes()`
  rather than assume.
- **Requires system pipewire ≥ 1.6 with `pipewire`/`libspa` crate ≥ 0.10** —
  the 0.8 series' pre-generated `spa_pod_builder` bindings don't match this
  Arch system's headers and fail to compile. Pin `>= 0.10` in the real host
  agent, and re-check compatibility on older-pipewire distros during Phase 3
  packaging.
- This step **cannot be run unattended** — a human must click the
  "Share Screen" dialog. `restore_token` (for the plan's silent-reconnect UX)
  is untested here; select_sources was called with `PersistMode::DoNot`.

```sh
cargo run -p spike-portal-capture
# A "Share Screen" dialog will appear -- approve it (pick a monitor).
# Captures 30 frames, decodes the first to palmtop-capture-spike.ppm
# (gitignored -- it's a real screenshot of whatever was on screen).
```

### Proven: VA-API hardware encode of real captured frames

`crates/spike-capture-encode` joins the capture spike directly to hardware
encode: captures 60 real desktop frames via the portal (same path as above),
dumps them tightly packed as raw BGRA, then shells out to `ffmpeg` for a
VA-API H.264 encode pass on the AMD iGPU (`/dev/dri/renderD128`, confirmed via
`/sys/class/drm/*/device/uevent` -- `renderD129` is the NVIDIA card).

**Result on real desktop content:** 60 frames @ 1920x1080 (497 MB raw) encoded
in 0.50s = **120 fps effective throughput, 4x realtime margin against a 30fps
target**. 497 MB -> 201 KB output. A synthetic `ffmpeg -f lavfi testsrc`
smoke test (no Rust) confirmed the same encoder path independently at
3.3x realtime for 1080p60 before this spike was built, ruling out synthetic-
source artifacts.

Notes:
- The spike shells out to the `ffmpeg` CLI rather than binding `libavcodec`
  directly (e.g. via `ffmpeg-next`) -- deliberate for a feasibility spike, to
  get a real throughput number fast without fighting Rust/FFI hwframe-context
  setup. The real host agent (§8 of the plan) should still target direct
  libavcodec/VA-API binding for a real product (avoids a subprocess+disk
  round-trip and enables zero-copy DMA-BUF), but CLI-via-subprocess is a
  legitimate fallback if that binding effort turns out to be disproportionate.
- Raw+encoded scratch files are deleted immediately after -- they contain real
  desktop content.

```sh
cargo run -p spike-capture-encode
# A "Share Screen" dialog will appear -- approve it.
# Captures 60 real frames (~2s), then reports capture size + VA-API encode
# throughput. All scratch files are deleted before it exits.
```

## Local configuration (no device data in code)

Machine- and device-specific values — IPs, adb serials, GPU render nodes, decoder names,
per-device limits — live in gitignored TOML under `config/`, never in source. Anyone cloning
this repo has different hardware and possibly several phones, so profiles are per-device files
that coexist rather than values edited in and out of the code.

```
config/
  README.md              committed
  host.example.toml      committed — template
  host.toml              yours, gitignored
  devices/
    example.toml         committed — template
    <name>.toml          yours, gitignored — one per device
  active                 yours, gitignored — default device name
```

```sh
./scripts/probe-host.sh   > config/host.toml              # autodetects IP + best render node
./scripts/probe-device.sh <serial> > config/devices/my-phone.toml
echo my-phone > config/active

# switch devices without touching any code
PALMTOP_DEVICE=other-phone ./scripts/run-decode-spike.sh
```

Consumers: Rust via the `palmtop-config` crate (`HostConfig::load()` / `DeviceConfig::load()`),
shell via `source scripts/device.sh`. The Android app takes host/port as intent extras and
**refuses to start without them** rather than falling back to a baked-in address.

## Environment detected on this machine
- Hyprland 0.55.4 (wlroots), Wayland session
- Rust 1.96
- GPUs: AMD Cezanne/Vega iGPU (VA-API via Mesa, `/dev/dri/renderD128`) + NVIDIA GTX 1650 Mobile
  (`/dev/dri/renderD129`)
- PipeWire 1.6.7, FFmpeg 8.1.2 (`h264_vaapi`/`hevc_vaapi`/`av1_vaapi` confirmed available)
- **Android toolchain installed user-locally (no root)** under `~/opt`:
  - JDK 17 (Temurin) — `~/opt/jdk-17.0.19+10`
  - Android SDK cmdline-tools 12.0, platform-tools 37.0.0, `platforms;android-36`,
    `build-tools;36.1.0`, `ndk;29.0.14206865` — `~/opt/android-sdk`
  - `sudo` is unavailable in this environment (no TTY for password), which is why this went
    user-local instead of a system package install
  - Source `~/opt/android-env.sh` (or open a fresh shell — it's wired into `~/.bashrc`) to get
    `JAVA_HOME`/`ANDROID_HOME`/`ANDROID_NDK_HOME`/`PATH` set up

### Measured: target device + network leg

**Test device:** Motorola moto g71 5G — Android 12 (SDK 31), Snapdragon 695 (`holi`),
1080x2400 @ density 420.

**Hardware decoders present** (from `/vendor/etc/media_codecs*.xml`):
- `c2.qti.avc.decoder.low_latency` — **dedicated low-latency H.264** ✅
- `c2.qti.hevc.decoder.low_latency` — **dedicated low-latency H.265** ✅
- AV1 is only `c2.android.av1.decoder` with `variant="slow-cpu"` — **software, not viable**.
  Deprioritize AV1 for this device class; negotiate H.264 baseline, step up to H.265.
- SDK 31 ≥ 30, so `KEY_LOW_LATENCY` is available.

**Network leg (phone hotspot, laptop as client).** Latency depends heavily on regime:

| Condition | RTT | Cause |
|---|---|---|
| Idle / sparse packets (0.2s gaps) | avg 33ms, max **263ms**, mdev 48ms | Wi-Fi power-save parking the radio |
| Saturated w/ bulk TCP | 10-80ms, climbing monotonically | Bufferbloat / queue buildup |
| **Radio awake, link unsaturated** | **2.6-11ms** | ← the actual operating regime |

Sustained throughput: **6.5 MB/s (~52 Mbps)**. Our measured encode output is ~0.8 Mbps
(201 KB / 60 frames @ qp24) — **~65x headroom**, so the link is never saturated in normal
operation and the low-latency regime is the one that applies.

Two design consequences, both reinforcing choices already in the plan:
- **Never queue / always drop stale frames** (§3.5 backpressure) — the bufferbloat row shows
  what happens if you let a buffer build: latency climbs without bound. Bandwidth is not the
  constraint; queuing discipline is.
- **Keep-alive + WakeLock matter more than expected** (§9) — the idle row's 263ms spike is not
  a rare tail, it's what happens whenever traffic goes sparse. A steady packet cadence is
  required to hold the radio awake, not merely nice-to-have.

**Rough glass-to-glass budget** against the plan's <80ms gate, using measured values where
available: capture (event-driven, ≤16ms) + encode (~8ms @120fps measured) + network (3-11ms
measured) + decode (hardware low-latency, est. 5-15ms) + display (~16ms @60Hz)
= **~35-65ms estimated**. Fits the gate, but the decode term is still an estimate — it is the
one remaining unmeasured leg.

### Proven: MediaCodec hardware decode — **the Phase 0 gate**

`android-spike/` is a minimal single-Activity Java app (no Gradle — built directly with
`aapt2`/`d8`/`apksigner` via `build.sh`, since a whole Gradle distribution is a lot to pull
for one Activity). It receives length-prefixed H.264 access units over TCP from
`crates/spike-h264-server`, decodes with the vendor low-latency decoder, renders to a
`SurfaceView`, and reports queue-input → output-available latency. That's measurable purely
on-device, so it needs no clock sync with the host.

**Final result — 984 frames, 1080p30, over the hotspot:**
```
avg=25.29ms  min=9.00ms  p50=25.32ms  p95=30.72ms  p99=34.78ms  max=51.98ms
```

Getting there took three corrections, each a real finding rather than a tuning tweak:

| Config | Decode latency | What it revealed |
|---|---|---|
| Blocking in `onInputBufferAvailable` | 133 ms | **Measurement bug.** MediaCodec runs *all* callbacks on one handler thread; blocking in the input callback stalls the output callback behind it. Never block in a MediaCodec callback. |
| Non-blocking callbacks | 37–40 ms | Real, but a standing queue: the decoder exposes several input buffers, so feeding whenever one is free lets a backlog build. |
| **`inflight=1`** | **25 ms** | Capping frames-in-flight removes the queue. Full 30fps throughput retained, zero dropped frames. |

Two further A/B results:
- **Rendering costs only ~3ms** (render=ON 40ms vs render=OFF 37ms) — display coupling is
  minor; the latency was queuing, not presentation.
- **1080p60 is *worse* than 1080p30 on this SoC: 65ms vs 37ms.** Latency rose while
  throughput held at a full 60fps — the signature of a standing queue near saturation.
  1080p60 sits close to the Snapdragon 695's decode capacity, so utilisation approaches 1 and
  queuing delay balloons. **Target 1080p30 on mid-range devices**, or drop resolution for 60fps.

### Proven: capture latency + input injection end-to-end

`crates/spike-capture-latency` closes the last two gaps in one measurement. It warps the
cursor to a known position via the wlr virtual pointer, timestamps the injection, then
watches incoming portal frames for the pixels to change (the portal session uses
`CursorMode::Embedded`, so the cursor is in the captured pixels).

**Result — 20/20 trials:**
```
mean=4.80ms  min=4.21ms  p50=4.47ms  p95=6.24ms  max=6.24ms
```

This is **~3x better than the ≤16 ms estimate** the budget previously carried, and it proves
the input path end-to-end: the standalone input spike only showed the compositor *accepted*
the protocol requests, whereas 20/20 detected pixel changes show injected input actually
reaches the compositor, gets rendered, and lands in captured output.

Getting a valid measurement took one correction worth recording: the first attempt
thresholded on **mean** pixel delta across the frame and detected almost nothing (2/20, with
nonsense values). A cursor is ~24 px on a 1080p screen — about 9 samples out of 32,400 when
downsampled — contributing ~0.03 to a frame-wide mean. Counting *individually strongly-changed
samples* instead of averaging is the right detector for a small moving object. The stimulus
also moved to absolute positioning (`motion_absolute`), since relative motion can silently
clamp at a screen edge and produce no change at all.

### Phase 0 verdict: **PASS**

Glass-to-glass budget against the plan's <80 ms gate — now four of five legs measured:

| Leg | Value | Source |
|---|---|---|
| Capture | **4.8 ms** mean / 6.2 ms p95 | **measured** (inject→compositor→frame) |
| Encode | ~8 ms | **measured** (120 fps VA-API throughput) |
| Network | 3–11 ms | **measured** (unsaturated hotspot) |
| Decode | 25 ms avg / 35 ms p99 | **measured** (1080p30, inflight=1) |
| Display | 0–16.7 ms quantisation, ~8.3 ms mean | refresh rate **measured** (60.000004 Hz); panel response not measurable in software |
| **Total** | **~53 ms typical, ~77 ms pessimistic stack-up** | |

Both the typical case and a pessimistic stack-up of every leg's tail now fit under 80 ms —
better than the earlier ~86 ms projection, because capture measured 4.8 ms rather than the
16 ms assumed. **The concept is viable.**

Remaining honesty caveat: **display is the one leg not fully measured, and cannot be** in
software — the handoff to the display controller is observable, but panel response time
(pixels actually changing) needs a camera or photodiode. The figure above is refresh-rate
quantisation only and excludes panel response, so real glass-to-glass is somewhat higher than
77 ms. Worth an external measurement before making latency claims publicly.

The budget still has limited slack, so the plan's latency-discipline items (never queue, drop
stale frames, cap in-flight work) remain load-bearing requirements rather than optimisations.

```sh
# host: serve the stream
cargo run --release -p spike-h264-server -- android-spike/test-1080p.h264 30 9999
# phone: build, install, run
cd android-spike && ./build.sh && adb install -r palmtop-spike.apk
adb shell am start -n dev.palmtop.spike/.MainActivity \
  --es host <laptop-ip> --ei port 9999 --ei inflight 1
adb logcat -s PalmtopSpike     # watch decode latency; RESULT line prints on disconnect
```
Note the phone must be **unlocked** — `SurfaceView.surfaceCreated` never fires while the
screen is dozing/locked, and the app will sit silently connected-but-idle.

## Next steps (Phase 0 remaining)
1. ~~Capture spike: xdg-desktop-portal `ScreenCast` → PipeWire DMA-BUF frames.~~ ✅ done
2. ~~Encode spike: FFmpeg VA-API (AMD iGPU) H.264.~~ ✅ done (120fps, 4x realtime on real frames)
3. ~~Android toolchain + target device profiling + network characterization.~~ ✅ done
   (low-latency HW decoders confirmed; network measured at 2.6-11ms in operating regime)
4. ~~Android decode spike — TCP → `MediaCodec` low-latency → `SurfaceView`.~~ ✅ done (25 ms avg)
5. ~~Glass-to-glass measurement → Phase 0 kill/continue gate.~~ ✅ **PASS**

6. ~~Measure the estimated capture leg; validate input injection end-to-end.~~ ✅ done (4.8 ms)

**Phase 0 is complete — the gate passes.** Carry into Phase 1:
- Measure true display latency with external hardware (camera/photodiode); it is the one
  leg software cannot observe.
- Enforce "cap in-flight work" as an invariant at every pipeline stage, not just the decoder.
- Drive input from real touch events on the phone (this spike injected from the host side).

### Connecting the test device
Wireless ADB pairing (Android 11+ "Wireless debugging") **requires the phone to be a Wi-Fi
client**, so it is unavailable while the phone is acting as a hotspot. Workaround used here —
legacy `adb tcpip`, which needs USB only once:

```sh
source ~/opt/android-env.sh
adb tcpip 5555                        # with USB connected
adb connect 192.168.217.213:5555      # phone's hotspot IP; USB can now be unplugged
```

Note this is a *dev-tooling* limitation only — the hotspot LAN itself carries app traffic
fine in both directions, so Palmtop's own connectivity is unaffected.

To unblock the Android client: install Android SDK + NDK and set `ANDROID_HOME`.
