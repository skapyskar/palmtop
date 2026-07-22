# Streaming sync: measurement, latency fixes, and quality modes

Date: 2026-07-22
Status: implemented and measured (see "Measured results" at the end)

## Problem

The pipeline currently keeps the phone and laptop in sync by always decoding the
*latest* frame and dropping stale ones. That works — it fixed a measured ~24s
progressive lag — but it treats a symptom. Three things are still missing:

1. **Nothing is measured.** There is no end-to-end latency number, no RTT, no
   per-stage timing. Every claim about responsiveness is currently an estimate
   assembled from Phase 0 component measurements, not an observation of the
   running system. This makes every further optimisation unfalsifiable.
2. **The encoder is constant-QP (`-qp 24`), not bitrate-capped.** Frame size is
   therefore unbounded: high-motion content produces large frames, and a large
   frame takes proportionally longer to push over Wi-Fi. This is the likely
   mechanism behind the video-playback lag observed live — the stale-frame drop
   handles the consequence, nothing handles the cause.
3. **There is one quality setting, baked into config.** No way to trade picture
   quality or battery for responsiveness.

## Non-goals

- Moving video off TCP to UDP/QUIC with FEC. TCP head-of-line blocking is a real
  structural limit, but Phase 0 measured ~65x bandwidth headroom and low loss on
  this LAN, so the penalty is plausibly small here. Deciding this needs the
  measurement layer below; building the transport first would be acting on a
  hunch. Explicitly revisited once numbers exist.
- Adopting WebRTC or GStreamer. WebRTC carries its own jitter buffer that works
  against a "never queue" design, and plan §2 already rejected forking
  Sunshine/Moonlight as the wrong abstraction for touch.
- True display (panel) latency. Still needs external hardware; unchanged since
  Phase 0. Reported numbers stay explicitly labelled as excluding it.

## Approach and why it is the long-term one

Four of the five workstreams below are transport-agnostic and survive any future
move to UDP: the measurement layer, the bitrate cap (an encoder setting),
frame-age dropping (which matters *more* under a lossy transport), and the mode
presets. Only the `SO_SNDBUF` bound is TCP-shaped, and it is a few lines. This is
the floor a future transport change would be built on, not scaffolding thrown
away by one.

---

## 1. Measurement foundation

Built first. Everything after it is unverifiable without it.

### 1.1 Clock offset and RTT

`Ping`/`Pong` already exist in the protocol and **neither side has ever sent
one** — the keepalive plan §9 called a primary requirement (because of the
measured 263 ms idle power-save RTT spike) is not running. Giving these messages
timestamps makes them earn their place twice: keepalive *and* the clock sync
every other measurement depends on.

Wire change:

```
Ping { nonce: u64, t_client_us: u64 }
Pong { nonce: u64, t_client_us: u64, t_host_recv_us: u64, t_host_send_us: u64 }
```

Client sends a burst of 5 pings at 200 ms on connect so the offset converges
within about a second, then settles to one per second. Without the burst the
first ~15 seconds of every session would report end-to-end numbers derived from a
barely-sampled offset, which is worse than reporting nothing.

With `t0 = t_client_us`, `t1 = t_host_recv_us`,
`t2 = t_host_send_us`, and `t3` = local receipt time:

```
rtt    = (t3 - t0) - (t2 - t1)
offset = ((t1 - t0) + (t2 - t3)) / 2     // offset = host_clock - client_clock
```

To convert a host timestamp `H` into client time: `H - offset`.

Both sides use a **monotonic** clock, never wall clock (`Instant`-derived
microseconds host-side, `System.nanoTime()` client-side). The arbitrary and
differing epochs are absorbed by `offset`.

Keep a rolling window of 16 samples and **use the offset from the minimum-RTT
sample**, not the mean. The lowest-RTT sample is the one with least queuing in
either direction, and therefore the least asymmetry error — this is the standard
NTP technique and matters more on Wi-Fi than on wired links.

**Honest limitation, to be stated wherever the number is displayed:** this
assumes symmetric path delay. Real asymmetry of `A` produces a one-way error of
`A/2`. On a LAN with min-RTT selection this should be a few milliseconds, but the
resulting end-to-end figure is an *estimate with a few-ms uncertainty*, not a
measurement. This is the same standard of honesty already applied to the display
leg; it must not quietly become "we measured 45ms".

### 1.2 Per-frame timestamps

`VideoFrame` gains `capture_us`: the daemon's monotonic clock at the moment the
capture thread received the frame from PipeWire.

This excludes compositor→PipeWire latency, which Phase 0 measured separately at
4.8 ms mean. Document that; do not silently omit it.

**Carrying the timestamp through ffmpeg.** Frames enter ffmpeg as raw pixels on
stdin and leave as Annex-B on stdout — a subprocess pipe carries no side-channel
metadata. Because the encoder runs with `-bf 0` (no B-frames) and input rate
equals output rate, the correspondence is strictly 1:1 and in order, so a shared
FIFO works: `run_feeder` pushes a timestamp when it writes a frame to stdin,
`run_reader` pops one when it emits an access unit.

That FIFO is a silent-corruption risk if the 1:1 assumption ever breaks (an
ffmpeg version that drops or duplicates frames would desync it, and every
subsequent timestamp would be wrong without any error). Guard it: if the queue
depth exceeds a small bound (say 8), log loudly and reset rather than reporting
confidently wrong numbers.

### 1.3 What gets reported

Client-side, per frame: `e2e = decoder_output_time - (capture_us - offset)`, plus
decode time, and arrival-to-decode. Aggregated as p50/p95 over a rolling window.

Host-side, logged periodically: capture→encode-publish, publish→socket-write.

**Input→screen latency is derived, not measured.** Detecting an injected input's
appearance requires pixel readback the client cannot cheaply do. It is computed
as `rtt/2 + inject + (measured video e2e)` and labelled as derived. Phase 0
already measured the inject+render+capture portion at 4.8 ms.

### 1.4 Surfaces

- **HUD** — small toggleable overlay on the phone: e2e p50/p95, rtt p50, decode
  p50, drop %, measured bitrate, mode, resolution, fps. For seeing a mode change
  take effect immediately.
- **`scripts/measure-latency.sh`** — runs each mode for a fixed duration,
  collects the client's reported samples, prints a percentile table. Video
  latency depends on screen content, so the script takes a required
  `--content "<description>"` argument and records it in the output; runs are
  only comparable when content is held constant. Saying so in the tool beats
  discovering it after comparing two incomparable runs.

---

## 2. Latency fixes

Each lands and is measured independently, so it is clear which one actually paid.

### 2.1 Bitrate cap replacing constant QP

Replace `-qp N` with CBR-style rate control: `-b:v` = `-maxrate` = the mode's
cap, and `-bufsize` = 100 ms of that cap (e.g. 800 kbit at 8 Mb/s, roughly three
average frames). A small VBV window is what actually bounds per-frame size, and
therefore per-frame transmit time. This is the direct fix for high-motion lag.

The exact VA-API incantation needs verifying against the driver during
implementation rather than assumed — `h264_vaapi` may need `-rc_mode` set
explicitly, and silently falling back to a different rate-control mode would
undo the entire point of this change. Verify the encoder actually reports CBR,
do not just check that ffmpeg accepted the flags.

### 2.2 Bound the kernel socket send buffer

`LatestEncoded` carefully drops stale frames, then hands the survivor to a kernel
send buffer that may hold several more — where nothing can drop them. **The
"never queue" invariant currently stops at the kernel boundary.** This is the one
pipeline stage never examined, and it is invisible from userspace without
looking for it.

Set `SO_SNDBUF` (via `socket2`, already a transitive dependency) to roughly
100 ms of the mode's bitrate cap, floor 64 KB. A full buffer makes `write_all`
block, which is correct: that is backpressure, and `LatestEncoded` keeps dropping
stale frames while the writer is blocked, which is exactly the desired behaviour.

Whether this helps is an empirical question — which is the point of doing §1
first.

### 2.3 Frame-age dropping on the client

Replace the coarse `available() > 0` proxy with an age test against the mode's
drop budget, using `capture_us`. More principled, and works when a newer frame
exists but has not fully arrived.

**Keyframes are never dropped**, unchanged. This is load-bearing: a previous
version dropped the opening keyframe along with its backlog, and the decoder then
accepted P-frames forever without ever producing output, starving the in-flight
permit. Keep the existing comment explaining this.

### 2.4 Send the pings

Fixes the keepalive gap in §1.1 — a primary requirement per plan §9 that turned
out never to have been wired up.

---

## 3. Quality modes

| | resolution | fps | maxrate | GOP | drop budget |
|---|---|---|---|---|---|
| **Sync** | 1280×720 | 30 | 6 Mb/s | 0.25 s | 40 ms |
| **Balanced** (default) | 1920×1080 | 30 | 8 Mb/s | 0.5 s | 80 ms |
| **Quality** | 1920×1080 | 30 | 16 Mb/s | 1 s | 150 ms |
| **Battery** | 1280×720 | 20 | 3 Mb/s | 1 s | 120 ms |

Sync starts at **30 fps, not 60**. Phase 0 found 1080p60 measured *worse* than
1080p30 (65 ms vs 37 ms) by pushing the Snapdragon 695 near its decode ceiling,
where utilisation approaches 1 and queuing latency balloons. 720p60 is a much
lighter load and may well win — but that is a measurement to run once §1 exists,
not an assumption to ship. The device profile's `[limits] max_fps` is the
authority per-device.

Resolution scaling happens on the GPU: `-vf format=nv12,hwupload,scale_vaapi=W:H`.

### UI

Mode selection and the HUD toggle both live in the existing on-screen control
row next to Reconnect and ⌨ — a `⚙` button opening a four-item picker, and a
`📊` toggle for the HUD. The selected mode persists in `SharedPreferences` and is
re-sent on reconnect, so a session resumed after an app restart comes back in the
mode the user chose rather than silently reverting to the default.

### Mode switching

Client sends `SetMode { mode }`. Host stops the encode stage, restarts ffmpeg
with the new arguments, sends a fresh `VideoConfig`, resumes. Restarting ffmpeg
naturally emits an IDR first, so the client always has a keyframe to resync on.

Client on receiving a `VideoConfig` whose resolution differs: tear down and
rebuild MediaCodec, then resume. **Because TCP is ordered, every frame after the
new `VideoConfig` is in the new resolution** — so the client simply ignores
`VideoFrame`s until its decoder is reconfigured, with no ambiguity about which
frames belong to which configuration. This is the fiddliest part of the change.

`PROTOCOL_VERSION` 2 → 3 (both `Ping`/`Pong` and `VideoFrame` change shape, and
`SetMode` is new).

---

## 4. Structure

`MainActivity.java` is already 804 lines and would balloon with a HUD, mode UI,
and decoder reconfigure. Extract:

- `LatencyTracker.java` — clock offset, RTT, rolling percentiles. Pure logic, no
  Android dependencies, so it is unit-testable.
- `HudView.java` — the overlay.
- `crates/palmtopd/src/modes.rs` — presets and preset→ffmpeg-args mapping.

---

## 5. Testing

**Unit:**
- Clock-offset math against synthetic samples, including deliberately asymmetric
  delay, verifying both the formula and that min-RTT selection picks the least
  skewed sample. Easy to get subtly wrong and impossible to eyeball later.
- Rolling percentile computation.
- Mode preset → ffmpeg argument mapping.
- The timestamp FIFO's desync guard.

**Integration:** extend `palmtop-test-client` to report measured latency, so most
measurement does not require the phone in hand.

**Live:** per-mode runs on the physical device via `measure-latency.sh`, holding
screen content constant.

---

## Open questions to settle with data, not opinion

1. Does 720p60 beat 720p30 for Sync on this device, or does it hit the same
   decode-ceiling wall 1080p60 did?
2. Does bounding `SO_SNDBUF` measurably help, or is the kernel queue already
   small enough to be irrelevant here?
3. After all of §2, what dominates the residual latency? If it is network stall
   rather than encode/decode, that is the evidence that would justify
   reconsidering UDP/QUIC.

---

# Measured results (2026-07-22)

Moto G71 5G over the phone's own hotspot, 25s per mode, palmtopd on Arch/Hyprland.
Two content workloads: a static desktop, and a fullscreen 1080p30 high-motion clip.

## Final numbers

| mode | resolution | e2e p50 | e2e p95 | rtt p50 | decode p50 |
|---|---|---|---|---|---|
| **sync** | 1280x720@30 | **52 ms** | 64 ms | 20 ms | 12 ms |
| balanced | 1920x1080@30 | 90 ms | 134 ms | 38 ms | 25 ms |
| quality | 1920x1080@30 | 88 ms | 141 ms | 36 ms | 25 ms |
| battery | 1280x720@20 | 63 ms | 91 ms | 21 ms | 16 ms |

Sync at 52 ms sits just under the ~53 ms Phase 0 predicted as typical, and well
inside the 80 ms gate. Modes do what they claim: the fast one is materially
faster, not cosmetically.

## The largest win was not a planned change

None of the three designed fixes moved the needle as much as discovering that
**the age-based drop rule specified in §2.3 was itself a regression.**

Steady-state e2e on this link is ~117 ms while the balanced budget is 80 ms, so
a pure age test discarded ~45% of frames *even when the pipeline was keeping up
and nothing newer was waiting behind them*. Those drops buy nothing back -- the
next frame is equally late -- they just discard frames already paid for in
bandwidth and decryption. Reverting to the original "drop only when a newer
frame is already buffered" rule:

| mode | e2e p50 age-rule | e2e p50 supersede-rule | rtt age-rule | rtt supersede |
|---|---|---|---|---|
| sync | 64 ms | **52 ms** | 29 ms | **20 ms** |
| balanced | 117 ms | **90 ms** | 114 ms | **38 ms** |
| quality | 183 ms | **88 ms** | 247 ms | **36 ms** |
| battery | 122 ms | **63 ms** | 120 ms | **21 ms** |

Up to 52% off end-to-end and 85% off RTT.

**Why RTT collapsed** is the interesting part, and it is a coupling nobody had
looked for: `feedDecoder` blocks on the `inFlight` semaphore, so decoding a
frame *stalls the socket read loop*. Decode and network drainage are not
independent stages -- decoding stale frames directly throttles how fast the
client drains its socket, which inflates the very RTT the drop rule was
reacting to. The supersede rule is self-correcting because it drops precisely
when a backlog exists, which is exactly when draining fast matters.

Generalises: **"is this work stale?" is the wrong question when the answer
cannot change the outcome. The right one is "is there better work right behind
it?"** A staleness budget below achievable latency is a throughput tax with no
latency benefit.

## The three open questions, answered

1. **Does 720p60 beat 720p30 for Sync?** No. 67 ms vs **52 ms** on identical
   content -- the same shape Phase 0 found at 1080p, just less severe. Framerate
   is the wrong axis to spend latency budget on here. Sync stays at 30fps.

2. **Does bounding `SO_SNDBUF` help?** Not for latency -- p50 stayed within
   run-to-run noise (±10-15 ms) in every mode, on both workloads. But under
   high-motion 1080p it roughly **halves the drop rate** (balanced 39.8% ->
   22.4%, quality 60.5% -> 34.2%) at identical latency: bounded, the writer
   blocks sooner and stale frames are discarded *before* being encrypted and
   transmitted. Kept, with the rationale corrected from the one predicted.

3. **What dominates residual latency?** Network RTT, and it is partly
   self-inflicted -- RTT scaled with our own send rate (6 Mb/s -> 29 ms,
   8 Mb/s -> 114 ms, 16 Mb/s -> 247 ms) before the drop-rule fix. After it,
   RTT sits at 20-38 ms and decode at 12-25 ms, so the two are now comparable
   and neither dominates. **This is not yet evidence for moving to UDP/QUIC.**
   The queueing that looked like a transport problem was a client-side
   scheduling problem, and fixing it recovered most of the loss.

## Encoder rate control: VBR, not CBR as spec'd

Measured before building on the assumption, and the assumption was wrong.

| rate control | static desktop (median frame) | high motion (max frame) |
|---|---|---|
| constant QP (previous) | 1.3 KB | **1,300,638 B** |
| CBR (as spec'd) | 33 KB | 90,600 B |
| **VBR (shipped)** | 6.7 KB | 90,600 B |

Constant QP's 1.3 MB worst-case frame needs ~200 ms of airtime by itself --
that is the video-playback lag mechanism, confirmed. But CBR pays for bounding
it by inflating *static* content 17x, and a desktop is mostly static. VBR bounds
the tail identically at a quarter of the idle cost.
