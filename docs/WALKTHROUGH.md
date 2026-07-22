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

**Test device:** a mid-range Android 12 phone (SDK 31, Snapdragon 695),
1080x2400 @ density 420. The chipset is what makes the numbers below
meaningful -- they are mid-range figures, not flagship ones -- so it is
named where the exact model is not.

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
adb connect <phone-ip>:5555           # USB can now be unplugged
```

Note this is a *dev-tooling* limitation only — the hotspot LAN itself carries app traffic
fine in both directions, so Palmtop's own connectivity is unaffected.

---

## Status: Phase 1 (MVP core) — **complete**

Real product code now, not spikes: continuous live capture → encode → network stream →
decode, with tier-2 input round-tripping from a real touchscreen, mDNS discovery, QR/token
pairing, and a systemd `--user` service. Verified live end-to-end from a physical phone.

| Component | What it is |
|---|---|
| `crates/palmtop-proto` | Versioned TLV wire protocol shared by host + client (6 tests) |
| `crates/palmtopd` | Host daemon: `capture` + `encode` + `input` + `session` modules, `pairing` (mDNS/QR) |
| `android-spike/` (app id `dev.palmtop.spike`) | Real Android client — direct-touch input, continuous MediaCodec decode, persisted host/token, in-app Reconnect |
| `systemd/palmtopd.service` + `scripts/install-service.sh` | `--user` service, no root |

### Architecture as built
```
palmtopd (systemd --user service)
  capture.rs   portal + PipeWire, continuous, single-slot FrameSlot (drop stale, never queue)
  encode.rs    persistent ffmpeg (VA-API), incremental Annex-B splitter, LatestEncoded mailbox
  input.rs     wlr virtual-pointer/keyboard, long-lived, driven by live network events
  session.rs   single-client TCP session: handshake -> pairing check -> wires the above together
  pairing.rs   mDNS advertise (_palmtop._tcp) + QR-rendered connect info (host:port:token)
        |
        | palmtop-proto (TLV over TCP)
        v
Android client (dev.palmtop.spike)
  Protocol.java     Java mirror of the wire format (DataInput/OutputStream are already
                     big-endian, so no manual byte-order handling either side)
  MainActivity.java direct-touch input, MediaCodec low-latency decode, generation-counter
                     reconnect lifecycle, host/port/token persisted in SharedPreferences
```

### Real bugs found and fixed while wiring this together
Each of these produced a concrete, observed symptom — recorded because the fix generalizes:

1. **MediaCodec callback blocking → 133ms phantom latency** (found in Phase 0, re-confirmed
   applying the same pattern here): never block inside a MediaCodec callback; all callbacks
   share one handler thread.
2. **VA-API `async_depth` defaults trade latency for throughput.** Phase 0's encode spike
   measured *throughput* (120fps batch) and never per-frame latency. Live streaming exposed
   the gap: default pipelining depth added real input-to-screen lag. Fixed with
   `[encode] async_depth = 1` in `config/host.toml` — cheap to spend given the ~65x bandwidth
   and ~4x throughput headroom already measured.
3. **Dropped tokio runtime mid-session → possible portal/DBus teardown race.** `session.rs`
   used to create a `tokio::runtime::Runtime` per connection and drop it immediately after
   extracting the PipeWire fd. The portal's DBus connection lives inside that runtime;
   dropping it while the still-in-use PipeWire stream depends on the session staying open
   risked exactly the kind of "capture negotiates fine, then delivers nothing" failure
   observed during testing. Fixed: one runtime for the daemon's whole lifetime (`main.rs`).
4. **The real 24-second lag bug: no backpressure between encode and network.** `FrameSlot`
   (capture → encode) correctly drops stale frames, but the original encode → network path
   used an *unbounded* `mpsc` channel. If the client ever fell even briefly behind the
   encoder, frames piled up without limit and were sent later, in order, arbitrarily stale —
   at one point measured at ~24s of accumulated lag. Fixed by applying the exact same
   single-slot "keep only the latest" design (`encode::LatestEncoded`) to this stage too.
   **Lesson**: "never queue" has to be an invariant enforced at *every* pipeline stage, not
   just the one that happened to get measured first.
5. **A locked/dozing phone silently no-ops.** `SurfaceView.surfaceCreated` never fires while
   the screen is off, so the app connects but never decodes, with no visible error. Purely an
   operational gotcha during testing, not a code bug — worth remembering when it looks "stuck".
6. **Relaunching via the launcher icon lost host/port.** Sends a bare `ACTION_MAIN` Intent
   with no extras, so every relaunch after pressing back needed a fresh `adb shell am start`.
   Fixed: host/port/token persist to `SharedPreferences` on first successful launch; a
   generation-counter reconnect lifecycle (`MainActivity.startConnection()`/`teardown()`) also
   adds an in-app **⟳ Reconnect** button so recovering from any dropped session never needs
   adb again.

### UX pivot: trackpad → direct touch
The first input mode was trackpad-relative (drag a cursor, tap to click) — mirroring the
plan's §4.3 default. Live testing surfaced a real usability problem the design hadn't
anticipated: video round-trip latency (capture→encode→network→decode→display) makes a
relative cursor visibly *lag behind* the finger, which feels broken even when the underlying
input is fast. Switched to **direct absolute touch** (tap where you want to click, exactly
like a touchscreen — see `MainActivity.onTouch`'s doc comment): position is set by *where*
you touched, not by watching a cursor arrive, so it's correct-feeling regardless of video
latency. Host-side support (`PointerMotionAbsolute`) already existed and was already proven
by `spike-capture-latency`; this was a client-only change. Trackpad mode may return later
as a selectable option (plan §4.3 still wants both), but direct-touch is the default now.

### Explicitly deferred (not forgotten, scoped out on purpose)
- **True display-latency measurement** still needs external hardware (camera/photodiode) —
  unchanged from the Phase 0 finding. This is now the *only* remaining deferred item.

### In-app QR camera scanning — done (`QrScanActivity` + `QrOverlayView`)
CameraX + ML Kit, launched from the discovery screen's **📷 Scan QR code** button, returning
host/port/token/pubkey straight into the connect flow. Verified live end-to-end: scan →
Noise handshake → `h264_vaapi 1920x1080@30` streaming, from a factory-reset app install.

**The bug worth remembering, because it produced no error of any kind.** The first version
detected *nothing*. Camera opened, ML Kit initialised, TFLite delegates loaded, zero
exceptions anywhere in the logs — and zero barcodes, across repeated real attempts. Cause:
CameraX's default `ImageAnalysis` resolution is **640x480**. Our pairing URI carries a
64-hex-character Noise public key, which pushes the QR to ~57x57 modules; at 640x480 from a
normal holding distance each module landed on well under a pixel. Nothing was broken — the
decoder simply never had the detail. The whole failure was invisible in logs because a
correctly-functioning pipeline that sees nothing looks exactly like one pointed at a wall.

Generalisable lesson: **when a vision/decode stage reports nothing rather than failing, suspect
the input resolution before the algorithm.** Diagnosing it needed CameraX's *own* logs
(`primaryStreamSpec = StreamSpec{resolution=640x480}`), not the app's — so `QrScanActivity`
now logs its analysis resolution on the first frame, deliberately, so the next person never
has to go looking for that.

Three fixes, in descending order of how much each mattered:
1. **1080p analysis frames** via `ResolutionSelector` — the actual fix. Costs frame rate
   (ML Kit at 1080p runs well under 30fps on a Snapdragon 695), but `KEEP_ONLY_LATEST` turns
   that into dropped frames rather than queueing, and a few readable frames beat thirty
   unreadable ones. Same "never queue" invariant as everywhere else in this pipeline.
2. **ML Kit zoom suggestion** — when it sees a code that's present but too small, it asks for
   a zoom and we apply it, so standing slightly too far back self-corrects instead of the user
   having to find the right distance by trial and error.
3. **QR-only formats** + tap-to-focus (continuous AF hunts on a flat, evenly-lit screen at
   close range, which is exactly our case).

`QrOverlayView` draws a live outline on the detected code — green when it parses as a palmtop
pairing URI, white when it's a QR but not ours. That distinction is the point: "detected but
undecodable" and "detected nothing" were indistinguishable before, which is precisely what
made the original bug so opaque. Its coordinate mapping reimplements PreviewView's
`FILL_CENTER` transform exactly (ML Kit reports corners in *rotated* analysis-image space),
and both Preview and ImageAnalysis are pinned to one aspect ratio so a single transform is
valid for both streams.

**Host-side counterpart: palmtopd now also writes a scannable QR file.** The terminal QR uses
`unicode::Dense1x2`, which packs two vertical modules per character cell — non-square modules,
a few hundred physical pixels total. That is genuinely not camera-readable for a code this
dense, and pretending otherwise was half the problem. `pairing.rs::write_qr_svg` writes
`$XDG_RUNTIME_DIR/palmtop-pair.svg` (mode 0600, tmpfs — it embeds the pairing token, so it
should not outlive the session), and **`./scripts/show-pair-qr.sh`** puts it on screen
fullscreen. The terminal QR stays first in the output because when it works it's the fastest
path; the SVG is the one that reliably scans.

### Transport encryption (Noise Protocol) — done
Pattern: `Noise_NK_25519_ChaChaPoly_BLAKE2s` (client needs no static key of its own; the host's
static public key must be known ahead of time — exactly the QR/mDNS pairing model already
built). Wired into **both** ends: `palmtop-proto::noise` (Rust, `snow`) and
`android/.../NoiseTransport.java` (Java, Signal's `org.signal.forks:noise-java` fork) —
**verified with a real cross-language handshake**, Android talking to the Linux daemon, not
just each side's own unit tests. The session (video + input, not just the pairing token) is
now genuinely encrypted on the wire, not plaintext.

Real bug found and fixed while wiring this in, worth remembering as a class: **a shared
`Mutex`/lock guarding both encrypt and decrypt deadlocked the reader and writer threads**,
because the original `send`/`recv` combined crypto with *blocking network I/O* inside one
lock — the reader thread's blocked read (waiting on the peer) held the lock the writer thread
needed just to send `VideoConfig`. First symptom was the test client hanging indefinitely past
its own timeout. Fixed by splitting `encrypt_chunk`/`decrypt_chunk` (pure computation, safe
under a lock) from the actual socket I/O (must happen outside any shared lock) — applied
identically on both the Rust and Java sides, the second time by design rather than by
rediscovering the bug twice.

Honest gap, not hidden, and **not closed by the QR scanner landing**: the client still accepts
an mDNS-advertised public key through the discovery path, and mDNS is LAN-broadcast rather than
truly out-of-band. Camera scanning now makes a genuinely out-of-band source *available*, but
until mDNS-sourced keys stop being trusted (or only scan-sourced pairings count as pinned), a
user who taps a discovered host rather than scanning is still exposed to a spoofed
advertisement. That's a deliberate follow-up decision with a real usability cost either way —
see `pairing.rs`'s doc comment for exactly what it does and doesn't protect against.

### Client build: migrated to Gradle
`android/` is now the real, actively-maintained client project (Gradle 9.6.1 + AGP 9.3.0,
JDK 17, same debug keystore as before so `adb install -r` still works over old installs).
Moved off the manual `aapt2`/`d8`/`build.sh` approach specifically so real dependencies (like
`noise-java`) resolve normally. `android-spike/` is now historical/frozen — the Phase 0 spike
history and the manual-build reference, not where new work happens.

### Android-side discovery (`HostDiscovery.java`) — done
No more `adb`-launched Intents needed for a new setup. First launch with nothing persisted
shows a "Find a Palmtop host" screen: `NsdManager` browses for the real `_palmtop._tcp` service
the daemon advertises, resolves it to host:port, and a tap prompts for the pairing token before
connecting. **📷 Scan QR code** on the same screen skips the typing entirely (see the QR scanning
section above); manual host/port entry is offered alongside as a fallback for networks that
block multicast (plan §9).
Verified live: discovered the running daemon, resolved it correctly, connected, and streamed —
screenshotted at each step, not just "it compiled."

Known rough edge: the Reconnect/⌨ buttons are visually reachable underneath the discovery
overlay (a FrameLayout z-order/sizing quirk) — harmless (tapping Reconnect while no host is set
is a no-op) but worth a cleanup pass later.

### Try it
```sh
# one-time: config/host.toml exists, an Android device profile is set up (see above)
./scripts/install-service.sh      # builds + installs + starts palmtopd as a systemd --user service
./scripts/run-client.sh           # builds + installs + launches the real client on the phone
```
The daemon prints a QR code + host/port/token on every start (`journalctl --user -u palmtopd -f`
once installed as a service). Phone must be unlocked for the video surface to attach.

To unblock the Android client: install Android SDK + NDK and set `ANDROID_HOME`.
