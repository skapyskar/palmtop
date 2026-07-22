# Streaming Sync: Measurement, Latency Fixes, and Quality Modes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure real end-to-end streaming latency, fix the three things that inflate it, and expose four quality modes that trade picture quality and battery against sync.

**Architecture:** Protocol v3 adds timestamps to `Ping`/`Pong` (giving NTP-style clock-offset estimation) and a `capture_us` to every `VideoFrame`. The client converts host timestamps into its own clock and reports capture→displayed latency. On top of that measurement floor: bitrate-capped encoding replaces constant-QP, the kernel socket send buffer gets bounded so the "never queue" invariant reaches past userspace, and stale frames are dropped by age rather than by a byte-availability proxy. Four presets vary resolution, fps, bitrate cap, GOP length and drop budget.

**Tech Stack:** Rust (palmtop-proto, palmtopd), Java 17 / Android SDK 36 (client), ffmpeg + VA-API (encode), MediaCodec (decode), `socket2` (already a transitive dep), JUnit 4 (new, for pure-logic client tests).

## Global Constraints

- `PROTOCOL_VERSION` goes 2 → 3 in **both** `crates/palmtop-proto/src/lib.rs` and `android/app/src/main/java/dev/palmtop/spike/Protocol.java`. They must match or the handshake rejects the client.
- All timestamps are **monotonic** microseconds, never wall clock. Host: `Instant` elapsed against a process-start baseline. Client: `System.nanoTime() / 1000`.
- Wire format is big-endian throughout. Java's `DataInput`/`DataOutputStream` are big-endian by default and match Rust's `to_be_bytes` with no manual conversion.
- **Keyframes are never dropped**, at any stage, in any mode. A dropped keyframe leaves the decoder consuming P-frames forever without producing output, starving the in-flight permit. This has already been hit once for real.
- Never hold the Noise mutex across blocking I/O. Crypto under the lock, I/O outside it. This caused a total deadlock once already.
- Latency figures exclude the panel/display leg and carry a few-ms uncertainty from the clock-offset symmetry assumption. Label them as estimates wherever displayed; do not round this away into "we measured X".
- Do not commit unless the user asks. Commit steps below are written for when they do.

---

## File Structure

**Created:**
- `crates/palmtopd/src/modes.rs` — the four presets and their mapping to ffmpeg arguments. Pure data + pure function, no I/O, so it is unit-testable.
- `android/app/src/main/java/dev/palmtop/spike/LatencyTracker.java` — clock offset, RTT, rolling percentiles. **No Android imports**, so it runs under a plain JVM unit test.
- `android/app/src/main/java/dev/palmtop/spike/HudView.java` — the stats overlay.
- `android/app/src/test/java/dev/palmtop/spike/LatencyTrackerTest.java` — JVM unit tests.
- `scripts/measure-latency.sh` — per-mode benchmark driver.

**Modified:**
- `crates/palmtop-proto/src/lib.rs` — v3 messages.
- `crates/palmtopd/src/capture.rs` — stamp frames at arrival.
- `crates/palmtopd/src/encode.rs` — timestamp FIFO, bitrate-capped spawn args.
- `crates/palmtopd/src/session.rs` — `SetMode` handling, encoder restart, `SO_SNDBUF`.
- `crates/palmtopd/src/main.rs` — module registration.
- `android/app/build.gradle.kts` — JUnit test dependency.
- `android/app/src/main/java/dev/palmtop/spike/Protocol.java` — v3 mirror.
- `android/app/src/main/java/dev/palmtop/spike/MainActivity.java` — ping loop, age-based drop, HUD + mode UI, decoder reconfigure.

---

### Task 1: Protocol v3 — timestamped Ping/Pong, frame timestamps, SetMode

**Files:**
- Modify: `crates/palmtop-proto/src/lib.rs`

**Interfaces:**
- Produces: `Message::Ping { nonce: u64, t_client_us: u64 }`, `Message::Pong { nonce: u64, t_client_us: u64, t_host_recv_us: u64, t_host_send_us: u64 }`, `Message::VideoFrame { keyframe: bool, capture_us: u64, data: Vec<u8> }`, `Message::SetMode { mode: u8 }`, `PROTOCOL_VERSION == 3`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block in `crates/palmtop-proto/src/lib.rs`:

```rust
#[test]
fn ping_pong_roundtrip_carries_timestamps() {
    match roundtrip(Message::Ping { nonce: 7, t_client_us: 123_456 }) {
        Message::Ping { nonce, t_client_us } => {
            assert_eq!(nonce, 7);
            assert_eq!(t_client_us, 123_456);
        }
        other => panic!("expected Ping, got {other:?}"),
    }
    match roundtrip(Message::Pong {
        nonce: 7,
        t_client_us: 123_456,
        t_host_recv_us: 200_000,
        t_host_send_us: 200_050,
    }) {
        Message::Pong { nonce, t_client_us, t_host_recv_us, t_host_send_us } => {
            assert_eq!(nonce, 7);
            assert_eq!(t_client_us, 123_456);
            assert_eq!(t_host_recv_us, 200_000);
            assert_eq!(t_host_send_us, 200_050);
        }
        other => panic!("expected Pong, got {other:?}"),
    }
}

#[test]
fn video_frame_roundtrip_carries_capture_timestamp() {
    let payload = vec![0u8, 0, 0, 1, 0x65, 0xAA, 0xBB];
    match roundtrip(Message::VideoFrame {
        keyframe: true,
        capture_us: 987_654_321,
        data: payload.clone(),
    }) {
        Message::VideoFrame { keyframe, capture_us, data } => {
            assert!(keyframe);
            assert_eq!(capture_us, 987_654_321);
            assert_eq!(data, payload);
        }
        other => panic!("expected VideoFrame, got {other:?}"),
    }
}

#[test]
fn set_mode_roundtrips() {
    match roundtrip(Message::SetMode { mode: 2 }) {
        Message::SetMode { mode } => assert_eq!(mode, 2),
        other => panic!("expected SetMode, got {other:?}"),
    }
}

#[test]
fn protocol_version_is_three() {
    assert_eq!(PROTOCOL_VERSION, 3);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p palmtop-proto`
Expected: FAIL — compile errors, `Message::Ping` has no field `t_client_us`, no variant `SetMode`.

- [ ] **Step 3: Bump the version constant**

In `crates/palmtop-proto/src/lib.rs` line 20:

```rust
/// v3: Ping/Pong carry timestamps (clock-offset estimation + the keepalive
/// that plan §9 wanted and that v2 defined but never actually sent),
/// VideoFrame carries the capture timestamp for end-to-end latency
/// measurement, and SetMode selects a quality preset.
pub const PROTOCOL_VERSION: u16 = 3;
```

- [ ] **Step 4: Add the SetMode tag**

In the `Tag` enum, after `Pong = 12`:

```rust
    SetMode = 13,
```

- [ ] **Step 5: Update the Message enum**

Replace the `VideoFrame`, `Ping` and `Pong` variants and add `SetMode`:

```rust
    /// Host -> client. `keyframe` lets the client log/measure without parsing
    /// NALs. `capture_us` is the host's monotonic clock at the moment the
    /// capture thread received this frame from PipeWire -- the client converts
    /// it through the clock offset from Ping/Pong to compute end-to-end
    /// latency. It excludes compositor->PipeWire latency (measured separately
    /// at 4.8ms mean in Phase 0), so it is a lower bound on true capture time.
    VideoFrame { keyframe: bool, capture_us: u64, data: Vec<u8> },

    /// Client -> host: select a quality preset. `mode` is a
    /// `palmtopd::modes::Mode` discriminant; unknown values are rejected by
    /// the host rather than silently defaulting, so a version skew shows up
    /// as an error instead of the wrong picture quality.
    SetMode { mode: u8 },

    /// Either direction: idle-connection keepalive (plan §9 "Wi-Fi power save
    /// dropping packets") *and* the clock-sync probe every latency
    /// measurement depends on. `t_client_us` is the client's monotonic clock
    /// at send.
    Ping { nonce: u64, t_client_us: u64 },
    /// Host -> client reply. Echoes `t_client_us` unchanged and adds the
    /// host's own monotonic clock at receive and at send, which is what makes
    /// the NTP offset formula work -- see LatencyTracker.java.
    Pong { nonce: u64, t_client_us: u64, t_host_recv_us: u64, t_host_send_us: u64 },
```

- [ ] **Step 6: Update write_to**

Replace the `VideoFrame`, `Ping`, `Pong` arms and add `SetMode`:

```rust
            Message::VideoFrame { keyframe, capture_us, data } => {
                payload.push(*keyframe as u8);
                payload.extend_from_slice(&capture_us.to_be_bytes());
                payload.extend_from_slice(data);
                Tag::VideoFrame
            }
            Message::SetMode { mode } => {
                payload.push(*mode);
                Tag::SetMode
            }
            Message::Ping { nonce, t_client_us } => {
                payload.extend_from_slice(&nonce.to_be_bytes());
                payload.extend_from_slice(&t_client_us.to_be_bytes());
                Tag::Ping
            }
            Message::Pong { nonce, t_client_us, t_host_recv_us, t_host_send_us } => {
                payload.extend_from_slice(&nonce.to_be_bytes());
                payload.extend_from_slice(&t_client_us.to_be_bytes());
                payload.extend_from_slice(&t_host_recv_us.to_be_bytes());
                payload.extend_from_slice(&t_host_send_us.to_be_bytes());
                Tag::Pong
            }
```

- [ ] **Step 7: Update read_from**

Replace the `VideoFrame`, `Ping`, `Pong` arms and add `SetMode`:

```rust
            t if t == Tag::VideoFrame as u8 => {
                let keyframe = read_u8(&mut p)? != 0;
                let capture_us = read_u64(&mut p)?;
                Message::VideoFrame { keyframe, capture_us, data: p.to_vec() }
            }
            t if t == Tag::SetMode as u8 => Message::SetMode { mode: read_u8(&mut p)? },
            t if t == Tag::Ping as u8 => Message::Ping {
                nonce: read_u64(&mut p)?,
                t_client_us: read_u64(&mut p)?,
            },
            t if t == Tag::Pong as u8 => Message::Pong {
                nonce: read_u64(&mut p)?,
                t_client_us: read_u64(&mut p)?,
                t_host_recv_us: read_u64(&mut p)?,
                t_host_send_us: read_u64(&mut p)?,
            },
```

- [ ] **Step 8: Fix the other crates that construct these variants**

`cargo build --workspace` will point at each. Expected sites:
- `crates/palmtopd/src/session.rs` — `Message::VideoFrame { keyframe: unit.keyframe, data: unit.data }` and the `Ping`/`Pong` arms.
- `crates/palmtopd/src/input.rs:131` — the ignore-list match arm.
- `crates/palmtop-test-client/src/main.rs` — the `VideoFrame` match arm.

For now make them compile with placeholder values (`capture_us: 0`); Tasks 4 and 6 give them real ones.

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS, all tests including the 4 new ones.

- [ ] **Step 10: Commit**

```bash
git add crates/palmtop-proto/src/lib.rs crates/palmtopd/src crates/palmtop-test-client/src
git commit -m "feat(proto): v3 adds frame + ping timestamps and SetMode"
```

---

### Task 2: Java protocol v3 mirror

**Files:**
- Modify: `android/app/src/main/java/dev/palmtop/spike/Protocol.java`

**Interfaces:**
- Consumes: the v3 wire format from Task 1.
- Produces: `Protocol.VERSION == 3`, `Protocol.ping(long nonce, long tClientUs)`, `Protocol.setMode(int mode)`, `Received.captureUs`, `Received.tClientUs`, `Received.tHostRecvUs`, `Received.tHostSendUs`, `Protocol.TAG_SET_MODE`.

- [ ] **Step 1: Bump the version and add the tag**

```java
    /** v3: Ping/Pong carry timestamps (clock sync + a keepalive that v2
     *  defined but never sent), VideoFrame carries capture_us, SetMode is new.
     *  Must equal palmtop-proto's PROTOCOL_VERSION or the handshake fails. */
    public static final int VERSION = 3;
```

And after `TAG_PONG`:

```java
    public static final int TAG_SET_MODE = 13;
```

- [ ] **Step 2: Replace the ping builder and add setMode**

Replace the existing `ping` method:

```java
    public static byte[] ping(long nonce, long tClientUs) {
        return frame(TAG_PING, p -> {
            p.writeLong(nonce);
            p.writeLong(tClientUs);
        });
    }

    public static byte[] setMode(int mode) {
        return frame(TAG_SET_MODE, p -> p.writeByte(mode));
    }
```

- [ ] **Step 3: Extend the Received POJO**

Add fields to `Protocol.Received`:

```java
        public long captureUs;
        public long tClientUs, tHostRecvUs, tHostSendUs;
```

- [ ] **Step 4: Update readMessage**

Replace the `TAG_VIDEO_FRAME`, `TAG_PING` and `TAG_PONG` cases:

```java
            case TAG_VIDEO_FRAME:
                r.keyframe = p.readUnsignedByte() != 0;
                r.captureUs = p.readLong();
                // 1 byte keyframe flag + 8 bytes timestamp, then the access unit.
                r.data = new byte[payload.length - 9];
                System.arraycopy(payload, 9, r.data, 0, r.data.length);
                break;
            case TAG_PING:
                r.nonce = p.readLong();
                r.tClientUs = p.readLong();
                break;
            case TAG_PONG:
                r.nonce = p.readLong();
                r.tClientUs = p.readLong();
                r.tHostRecvUs = p.readLong();
                r.tHostSendUs = p.readLong();
                break;
```

- [ ] **Step 5: Fix the caller in MainActivity**

`MainActivity.java` calls `Protocol.pong(msg.nonce)` in response to `TAG_PING`. The host no longer sends `Ping` (the client drives it in Task 7), so delete that branch — Task 7 replaces it with `TAG_PONG` handling.

- [ ] **Step 6: Build to verify it compiles**

Run: `cd android && JAVA_HOME=$HOME/opt/jdk-17.0.19+10 ./gradlew assembleDebug`
Expected: `BUILD SUCCESSFUL`

- [ ] **Step 7: Commit**

```bash
git add android/app/src/main/java/dev/palmtop/spike/Protocol.java android/app/src/main/java/dev/palmtop/spike/MainActivity.java
git commit -m "feat(client): mirror protocol v3"
```

---

### Task 3: LatencyTracker — clock offset, RTT, percentiles

**Files:**
- Create: `android/app/src/main/java/dev/palmtop/spike/LatencyTracker.java`
- Create: `android/app/src/test/java/dev/palmtop/spike/LatencyTrackerTest.java`
- Modify: `android/app/build.gradle.kts`

**Interfaces:**
- Produces: `new LatencyTracker()`, `void onPong(long tClientSendUs, long tHostRecvUs, long tHostSendUs, long tClientRecvUs)`, `long offsetUs()`, `boolean hasOffset()`, `void recordFrame(long e2eUs, long decodeUs)`, `void recordDrop()`, `Stats snapshot()` where `Stats` has `public long e2eP50, e2eP95, rttP50, decodeP50; public double dropPercent; public boolean valid;`

**Why this file has no Android imports:** it is pure arithmetic, and the offset formula is easy to get subtly wrong in a way no amount of staring at a running app would reveal. Keeping it dependency-free means it runs under a plain JVM unit test where asymmetric-delay cases can be constructed deliberately.

- [ ] **Step 1: Add the JUnit test dependency**

In `android/app/build.gradle.kts`, inside the existing `dependencies { ... }` block:

```kotlin
    // LatencyTracker is deliberately free of Android imports so its clock-offset
    // math can be tested on a plain JVM -- the formula is easy to get subtly
    // wrong and impossible to eyeball from a running app.
    testImplementation("junit:junit:4.13.2")
```

- [ ] **Step 2: Write the failing test**

Create `android/app/src/test/java/dev/palmtop/spike/LatencyTrackerTest.java`:

```java
package dev.palmtop.spike;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class LatencyTrackerTest {

    /** Simulates one round trip and feeds the result to the tracker.
     *  hostAhead is the true clock offset (host = client + hostAhead). */
    private static void exchange(LatencyTracker t, long clientSend, long upDelay,
                                 long hostProcess, long downDelay, long hostAhead) {
        long tHostRecv = clientSend + upDelay + hostAhead;
        long tHostSend = tHostRecv + hostProcess;
        long tClientRecv = clientSend + upDelay + hostProcess + downDelay;
        t.onPong(clientSend, tHostRecv, tHostSend, tClientRecv);
    }

    @Test
    public void noOffsetUntilFirstSample() {
        LatencyTracker t = new LatencyTracker();
        assertFalse(t.hasOffset());
    }

    @Test
    public void recoversOffsetExactlyWhenDelayIsSymmetric() {
        LatencyTracker t = new LatencyTracker();
        exchange(t, 1_000, 5_000, 200, 5_000, 7_000_000);
        assertTrue(t.hasOffset());
        assertEquals(7_000_000, t.offsetUs());
    }

    @Test
    public void rttExcludesHostProcessingTime() {
        LatencyTracker t = new LatencyTracker();
        exchange(t, 1_000, 5_000, 50_000, 5_000, 0);
        // 5ms up + 5ms down = 10ms, regardless of the 50ms the host held it.
        assertEquals(10_000, t.snapshot().rttP50);
    }

    /** The whole reason for min-RTT selection: a badly asymmetric sample
     *  skews the offset by half the asymmetry, so the tracker must prefer
     *  the least-delayed round trip rather than averaging them all in. */
    @Test
    public void prefersTheLeastDelayedSampleOverASkewedOne() {
        LatencyTracker t = new LatencyTracker();
        // Badly asymmetric: 60ms up, 2ms down. Naive averaging would put the
        // offset ~29ms off.
        exchange(t, 1_000, 60_000, 100, 2_000, 1_000_000);
        // Clean, symmetric, and lower RTT -- this is the one to trust.
        exchange(t, 200_000, 3_000, 100, 3_000, 1_000_000);
        assertEquals(1_000_000, t.offsetUs());
    }

    @Test
    public void percentilesOverRecordedFrames() {
        LatencyTracker t = new LatencyTracker();
        for (int i = 1; i <= 100; i++) t.recordFrame(i * 1_000L, 20_000);
        LatencyTracker.Stats s = t.snapshot();
        assertEquals(50_000, s.e2eP50);
        assertEquals(95_000, s.e2eP95);
        assertEquals(20_000, s.decodeP50);
    }

    @Test
    public void dropPercentCountsAgainstTotalFrames() {
        LatencyTracker t = new LatencyTracker();
        for (int i = 0; i < 9; i++) t.recordFrame(10_000, 5_000);
        t.recordDrop();
        assertEquals(10.0, t.snapshot().dropPercent, 0.001);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd android && JAVA_HOME=$HOME/opt/jdk-17.0.19+10 ./gradlew testDebugUnitTest`
Expected: FAIL — `LatencyTracker` does not exist.

- [ ] **Step 4: Implement LatencyTracker**

Create `android/app/src/main/java/dev/palmtop/spike/LatencyTracker.java`:

```java
package dev.palmtop.spike;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Deque;
import java.util.List;

/**
 * Estimates the host/client clock offset and tracks streaming latency
 * percentiles.
 *
 * Deliberately free of Android imports so it runs under a plain JVM unit test.
 * The offset formula below is standard NTP, and it is exactly the kind of
 * arithmetic that can be wrong by a factor of two or a sign without ever
 * looking wrong on screen -- a plausible-but-incorrect number is worse than no
 * number, because it gets quoted.
 *
 * <h3>The symmetry assumption, stated plainly</h3>
 * The offset estimate assumes the network delay is the same in both
 * directions. Real asymmetry {@code A} produces an offset error of {@code A/2},
 * which flows straight into every end-to-end figure derived from it. Two things
 * mitigate it and neither eliminates it: we keep a window of samples and use
 * the one with the *lowest* RTT (least queuing in either direction, therefore
 * least asymmetry), and LAN round trips are short to begin with. Treat the
 * resulting end-to-end number as an estimate with a few-ms uncertainty. It is
 * not a measurement, and it must not be presented as one.
 */
public final class LatencyTracker {
    private static final int OFFSET_WINDOW = 16;
    private static final int SAMPLE_WINDOW = 240; // ~8s at 30fps

    /** One round-trip probe: the offset it implies and how much it should be trusted. */
    private static final class Probe {
        final long offsetUs;
        final long rttUs;
        Probe(long offsetUs, long rttUs) { this.offsetUs = offsetUs; this.rttUs = rttUs; }
    }

    private final Deque<Probe> probes = new ArrayDeque<>();
    private final Deque<Long> e2eSamples = new ArrayDeque<>();
    private final Deque<Long> decodeSamples = new ArrayDeque<>();
    private long framesDecoded = 0;
    private long framesDropped = 0;

    /**
     * @param tClientSendUs client monotonic clock when the Ping went out
     * @param tHostRecvUs   host monotonic clock when it arrived
     * @param tHostSendUs   host monotonic clock when the Pong went out
     * @param tClientRecvUs client monotonic clock when the Pong arrived
     */
    public synchronized void onPong(long tClientSendUs, long tHostRecvUs,
                                    long tHostSendUs, long tClientRecvUs) {
        long rtt = (tClientRecvUs - tClientSendUs) - (tHostSendUs - tHostRecvUs);
        long offset = ((tHostRecvUs - tClientSendUs) + (tHostSendUs - tClientRecvUs)) / 2;
        if (rtt < 0) return; // clock went backwards or a reordered reply -- unusable
        probes.addLast(new Probe(offset, rtt));
        while (probes.size() > OFFSET_WINDOW) probes.removeFirst();
    }

    public synchronized boolean hasOffset() {
        return !probes.isEmpty();
    }

    /** Host clock minus client clock. Convert a host timestamp with {@code hostUs - offsetUs()}. */
    public synchronized long offsetUs() {
        Probe best = null;
        for (Probe p : probes) {
            if (best == null || p.rttUs < best.rttUs) best = p;
        }
        return best == null ? 0 : best.offsetUs;
    }

    public synchronized void recordFrame(long e2eUs, long decodeUs) {
        framesDecoded++;
        push(e2eSamples, e2eUs);
        push(decodeSamples, decodeUs);
    }

    public synchronized void recordDrop() {
        framesDropped++;
    }

    private static void push(Deque<Long> q, long v) {
        q.addLast(v);
        while (q.size() > SAMPLE_WINDOW) q.removeFirst();
    }

    public static final class Stats {
        public long e2eP50, e2eP95, rttP50, decodeP50;
        public double dropPercent;
        public boolean valid;
    }

    public synchronized Stats snapshot() {
        Stats s = new Stats();
        s.e2eP50 = percentile(e2eSamples, 50);
        s.e2eP95 = percentile(e2eSamples, 95);
        s.decodeP50 = percentile(decodeSamples, 50);
        List<Long> rtts = new ArrayList<>(probes.size());
        for (Probe p : probes) rtts.add(p.rttUs);
        s.rttP50 = percentileOf(rtts, 50);
        long total = framesDecoded + framesDropped;
        s.dropPercent = total == 0 ? 0.0 : (100.0 * framesDropped) / total;
        s.valid = hasOffset() && !e2eSamples.isEmpty();
        return s;
    }

    private static long percentile(Deque<Long> q, int pct) {
        return percentileOf(new ArrayList<>(q), pct);
    }

    /** Nearest-rank percentile. Exact and obvious; interpolation would buy nothing here. */
    private static long percentileOf(List<Long> values, int pct) {
        if (values.isEmpty()) return 0;
        Collections.sort(values);
        int rank = (int) Math.ceil(pct / 100.0 * values.size()) - 1;
        if (rank < 0) rank = 0;
        if (rank >= values.size()) rank = values.size() - 1;
        return values.get(rank);
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd android && JAVA_HOME=$HOME/opt/jdk-17.0.19+10 ./gradlew testDebugUnitTest`
Expected: PASS, 6 tests.

- [ ] **Step 6: Commit**

```bash
git add android/app/build.gradle.kts android/app/src/main/java/dev/palmtop/spike/LatencyTracker.java android/app/src/test
git commit -m "feat(client): LatencyTracker with unit-tested clock-offset math"
```

---

### Task 4: Stamp captured frames and carry the timestamp through ffmpeg

**Files:**
- Modify: `crates/palmtopd/src/capture.rs`
- Modify: `crates/palmtopd/src/encode.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `capture::Frame.capture_us: u64`, `capture::monotonic_us() -> u64`, `encode::TimestampFifo` with `new() -> Arc<Self>`, `push(&self, us: u64)`, `pop(&self) -> u64`, and `encode::EncodedUnit.capture_us: u64`.

**The problem this solves:** frames enter ffmpeg as raw pixels on stdin and leave as Annex-B on stdout. A subprocess pipe carries no side-channel metadata, so the timestamp has to be reunited with its frame on the far side. Because the encoder runs `-bf 0` (no B-frames) and input rate equals output rate, the correspondence is strictly 1:1 and in order — so a FIFO works. That assumption is load-bearing and silent if it breaks, hence the guard.

- [ ] **Step 1: Write the failing test**

Add to `crates/palmtopd/src/encode.rs` in its `#[cfg(test)] mod tests`:

```rust
#[test]
fn timestamp_fifo_is_first_in_first_out() {
    let fifo = TimestampFifo::new();
    fifo.push(10);
    fifo.push(20);
    assert_eq!(fifo.pop(), 10);
    assert_eq!(fifo.pop(), 20);
}

#[test]
fn timestamp_fifo_returns_zero_when_empty() {
    let fifo = TimestampFifo::new();
    assert_eq!(fifo.pop(), 0);
}

/// If ffmpeg ever stops emitting one access unit per input frame, the FIFO
/// silently desyncs and every timestamp after that point is wrong -- with no
/// error anywhere. Confidently wrong numbers are worse than missing ones, so
/// the queue self-resets rather than drifting.
#[test]
fn timestamp_fifo_resets_when_it_grows_past_the_bound() {
    let fifo = TimestampFifo::new();
    for i in 0..(FIFO_DESYNC_BOUND + 3) {
        fifo.push(i as u64);
    }
    assert!(fifo.len() <= FIFO_DESYNC_BOUND);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p palmtopd`
Expected: FAIL — `TimestampFifo` not found.

- [ ] **Step 3: Add the monotonic clock and stamp frames**

In `crates/palmtopd/src/capture.rs`, add near the top:

```rust
use std::sync::OnceLock;
use std::time::Instant;

/// Process-wide monotonic clock baseline. `Instant` has no numeric
/// representation, so timestamps are microseconds since the first call. The
/// epoch is arbitrary and differs from the client's -- that difference is
/// exactly what the Ping/Pong clock offset absorbs.
pub fn monotonic_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}
```

Add the field to `Frame`:

```rust
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: VideoFormat,
    /// Tightly packed (stride padding stripped), one row after another.
    pub bytes: Vec<u8>,
    /// `monotonic_us()` at the moment this frame was received from PipeWire.
    /// Excludes compositor->PipeWire latency, measured at 4.8ms mean in
    /// Phase 0 -- so end-to-end figures derived from it are a lower bound.
    pub capture_us: u64,
}
```

Then set `capture_us: monotonic_us()` at the single site in `capture.rs` where a `Frame` is constructed.

- [ ] **Step 4: Add TimestampFifo to encode.rs**

```rust
/// Upper bound on in-flight timestamps before the FIFO is assumed desynced.
/// `async_depth` is 1 and there are no B-frames, so more than a handful queued
/// means the 1:1 input-frame-to-access-unit assumption has broken.
pub const FIFO_DESYNC_BOUND: usize = 8;

/// Reunites a capture timestamp with its encoded frame across ffmpeg's stdin
/// and stdout pipes, which carry no metadata of their own.
///
/// Correct only because the encoder runs with `-bf 0` and input rate equals
/// output rate, making the correspondence strictly 1:1 and in order. That
/// assumption is worth guarding rather than trusting: an ffmpeg build that
/// dropped or duplicated a frame would desync this queue permanently, and
/// every subsequent latency figure would be wrong with nothing logged.
pub struct TimestampFifo {
    inner: Mutex<VecDeque<u64>>,
}

impl TimestampFifo {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(VecDeque::new()) })
    }

    pub fn push(&self, us: u64) {
        let mut q = self.inner.lock().unwrap();
        q.push_back(us);
        if q.len() > FIFO_DESYNC_BOUND {
            eprintln!(
                "[encode] timestamp FIFO exceeded {FIFO_DESYNC_BOUND} entries -- ffmpeg is not \
                 emitting one access unit per input frame. Latency numbers would be wrong; \
                 resetting the queue."
            );
            q.clear();
            q.push_back(us);
        }
    }

    /// Returns 0 if empty, which the client treats as "unknown" rather than
    /// as a timestamp -- see MainActivity's frame-age check.
    pub fn pop(&self) -> u64 {
        self.inner.lock().unwrap().pop_front().unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}
```

Add `use std::collections::VecDeque;` to the imports.

- [ ] **Step 5: Thread the timestamp through feeder and reader**

Add the field to `EncodedUnit`:

```rust
pub struct EncodedUnit {
    pub keyframe: bool,
    pub capture_us: u64,
    pub data: Vec<u8>,
}
```

Change `run_feeder`'s signature to take the FIFO and push before writing:

```rust
pub fn run_feeder(
    mut stdin: ChildStdin,
    slot: Arc<FrameSlot>,
    timestamps: Arc<TimestampFifo>,
    stop: Arc<AtomicBool>,
) {
```

and inside the loop, immediately before `stdin.write_all(&frame.bytes)`:

```rust
        timestamps.push(frame.capture_us);
```

Change `run_reader` to take the FIFO and pop per access unit:

```rust
pub fn run_reader(
    mut stdout: impl Read,
    latest: Arc<LatestEncoded>,
    timestamps: Arc<TimestampFifo>,
) {
```

and replace the publish line:

```rust
        for (keyframe, data) in splitter.push(&chunk[..n]) {
            latest.publish(EncodedUnit { keyframe, capture_us: timestamps.pop(), data });
        }
```

- [ ] **Step 6: Update the call sites in session.rs**

In `handle_client`, create the FIFO and pass it to both threads:

```rust
    let timestamps = encode::TimestampFifo::new();

    let feeder_handle = {
        let (slot, stop, timestamps) = (slot.clone(), stop.clone(), timestamps.clone());
        thread::spawn(move || encode::run_feeder(ffmpeg_stdin, slot, timestamps, stop))
    };
```

and:

```rust
    let reader_handle = {
        let (latest_encoded, timestamps) = (latest_encoded.clone(), timestamps.clone());
        thread::spawn(move || encode::run_reader(ffmpeg_stdout, latest_encoded, timestamps))
    };
```

In `run_writer`, use the real timestamp:

```rust
            let msg = Message::VideoFrame {
                keyframe: unit.keyframe,
                capture_us: unit.capture_us,
                data: unit.data,
            };
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets 2>&1 | grep -E "^error" ; echo "clippy clean"`
Expected: all tests PASS, no clippy errors.

- [ ] **Step 8: Commit**

```bash
git add crates/palmtopd/src
git commit -m "feat(host): carry capture timestamps through the encoder"
```

---

### Task 5: Quality mode presets

**Files:**
- Create: `crates/palmtopd/src/modes.rs`
- Modify: `crates/palmtopd/src/main.rs`
- Modify: `crates/palmtopd/src/encode.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `modes::Mode` (enum `Sync`, `Balanced`, `Quality`, `Battery`), `Mode::from_u8(u8) -> Option<Mode>`, `Mode::as_u8(self) -> u8`, `Mode::preset(self) -> Preset`, and `Preset { width, height, fps, maxrate_kbps, gop, drop_budget_ms, sndbuf_bytes }`.

- [ ] **Step 1: Write the failing test**

Create `crates/palmtopd/src/modes.rs` containing only its test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_discriminants_round_trip() {
        for m in [Mode::Sync, Mode::Balanced, Mode::Quality, Mode::Battery] {
            assert_eq!(Mode::from_u8(m.as_u8()), Some(m));
        }
    }

    /// An unknown discriminant must be rejected, not silently coerced to a
    /// default -- a version skew should surface as an error, not as the wrong
    /// picture quality that nobody notices.
    #[test]
    fn unknown_mode_is_rejected() {
        assert_eq!(Mode::from_u8(200), None);
    }

    #[test]
    fn sync_is_lower_latency_than_quality_on_every_axis() {
        let s = Mode::Sync.preset();
        let q = Mode::Quality.preset();
        assert!(s.width * s.height < q.width * q.height, "fewer pixels");
        assert!(s.gop < q.gop, "tighter GOP");
        assert!(s.drop_budget_ms < q.drop_budget_ms, "stricter staleness budget");
    }

    #[test]
    fn battery_is_the_cheapest_preset() {
        let b = Mode::Battery.preset();
        for other in [Mode::Sync, Mode::Balanced, Mode::Quality] {
            assert!(b.maxrate_kbps <= other.preset().maxrate_kbps);
            assert!(b.fps <= other.preset().fps);
        }
    }

    #[test]
    fn send_buffer_never_below_the_floor() {
        for m in [Mode::Sync, Mode::Balanced, Mode::Quality, Mode::Battery] {
            assert!(m.preset().sndbuf_bytes >= 64 * 1024);
        }
    }
}
```

- [ ] **Step 2: Register the module and run the test to verify it fails**

Add `mod modes;` to `crates/palmtopd/src/main.rs` alongside the other `mod` declarations.

Run: `cargo test -p palmtopd`
Expected: FAIL — `Mode` not found.

- [ ] **Step 3: Implement modes.rs**

Prepend to `crates/palmtopd/src/modes.rs`:

```rust
//! Quality presets: the knobs that trade picture quality and power against
//! sync, bundled into four named points rather than exposed individually.
//!
//! Why 30fps for `Sync` rather than 60: Phase 0 measured 1080p60 as *worse*
//! than 1080p30 end-to-end (65ms vs 37ms) on this class of device. Sixty
//! frames a second sat near the Snapdragon 695's decode ceiling, and once
//! utilisation approaches 1 the queuing latency balloons -- more frames
//! arrived, each one later. 720p60 is a far lighter load and may well win,
//! but that is a measurement to run, not an assumption to ship. The device
//! profile's `[limits] max_fps` remains the per-device authority.

/// Wire-stable discriminants. Do not renumber: they cross the protocol.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Sync = 0,
    Balanced = 1,
    Quality = 2,
    Battery = 3,
}

pub struct Preset {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub maxrate_kbps: u32,
    /// Keyframe interval in frames.
    pub gop: u32,
    /// How stale a frame may be, client-side, before it is skipped.
    pub drop_budget_ms: u32,
    /// Kernel socket send-buffer cap -- see session.rs for why this matters.
    pub sndbuf_bytes: usize,
}

/// Kernel send buffers below this are counterproductive: a single keyframe
/// would not fit, so the writer would stall mid-frame for no latency benefit.
const SNDBUF_FLOOR: usize = 64 * 1024;

/// Bound the kernel queue at roughly this much airtime. Any larger and stale
/// frames accumulate somewhere `LatestEncoded` cannot reach them.
const SNDBUF_MILLIS: usize = 100;

const fn sndbuf_for(maxrate_kbps: u32) -> usize {
    let bytes = (maxrate_kbps as usize * 1000 / 8) * SNDBUF_MILLIS / 1000;
    if bytes < SNDBUF_FLOOR { SNDBUF_FLOOR } else { bytes }
}

impl Mode {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// `None` for an unrecognised discriminant. Deliberately not defaulting:
    /// a client from a different protocol version should produce a visible
    /// error, not quietly get a mode it did not ask for.
    pub fn from_u8(v: u8) -> Option<Mode> {
        match v {
            0 => Some(Mode::Sync),
            1 => Some(Mode::Balanced),
            2 => Some(Mode::Quality),
            3 => Some(Mode::Battery),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Sync => "sync",
            Mode::Balanced => "balanced",
            Mode::Quality => "quality",
            Mode::Battery => "battery",
        }
    }

    pub fn preset(self) -> Preset {
        match self {
            Mode::Sync => Preset {
                width: 1280, height: 720, fps: 30, maxrate_kbps: 6000,
                gop: 8, drop_budget_ms: 40, sndbuf_bytes: sndbuf_for(6000),
            },
            Mode::Balanced => Preset {
                width: 1920, height: 1080, fps: 30, maxrate_kbps: 8000,
                gop: 15, drop_budget_ms: 80, sndbuf_bytes: sndbuf_for(8000),
            },
            Mode::Quality => Preset {
                width: 1920, height: 1080, fps: 30, maxrate_kbps: 16000,
                gop: 30, drop_budget_ms: 150, sndbuf_bytes: sndbuf_for(16000),
            },
            Mode::Battery => Preset {
                width: 1280, height: 720, fps: 20, maxrate_kbps: 3000,
                gop: 20, drop_budget_ms: 120, sndbuf_bytes: sndbuf_for(3000),
            },
        }
    }
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Balanced
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p palmtopd`
Expected: PASS, 5 new tests.

- [ ] **Step 5: Rewrite encode::spawn to take a preset and cap the bitrate**

Replace `encode::spawn` in `crates/palmtopd/src/encode.rs`:

```rust
/// Spawns the encoder for a given quality preset.
///
/// Rate control is CBR-style (`-b:v` == `-maxrate`) with a deliberately small
/// VBV buffer, replacing the constant-QP setup this started with. Constant QP
/// leaves frame size unbounded, so high-motion content produced large frames,
/// and a large frame takes proportionally longer to push over Wi-Fi -- the
/// likely mechanism behind the video-playback lag observed live. The stale-
/// frame drop handles the consequence of that; this handles the cause.
///
/// `src_width`/`src_height` are the compositor's real output size; the preset
/// may ask for something smaller, in which case VA-API scales on the GPU.
pub fn spawn(
    cfg: &palmtop_config::HostConfig,
    preset: &crate::modes::Preset,
    src_width: u32,
    src_height: u32,
) -> Result<Child> {
    let scale = if preset.width != src_width || preset.height != src_height {
        format!(",scale_vaapi={}:{}", preset.width, preset.height)
    } else {
        String::new()
    };
    // 100ms of airtime. Small VBV is what actually bounds per-frame size;
    // a large one would let the encoder spend a whole second's budget on one
    // frame and reintroduce exactly the problem this replaced.
    let bufsize_kbit = preset.maxrate_kbps / 10;

    Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-init_hw_device"])
        .arg(format!("vaapi=va:{}", cfg.gpu.vaapi_render_node))
        .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-s"])
        .arg(format!("{src_width}x{src_height}"))
        .args(["-r", &preset.fps.to_string(), "-i", "pipe:0"])
        .arg("-vf")
        .arg(format!("format=nv12,hwupload{scale}"))
        .args(["-c:v", &cfg.encode.codec])
        .args(["-rc_mode", "CBR"])
        .args(["-b:v", &format!("{}k", preset.maxrate_kbps)])
        .args(["-maxrate", &format!("{}k", preset.maxrate_kbps)])
        .args(["-bufsize", &format!("{bufsize_kbit}k")])
        .args(["-bf", "0", "-g", &preset.gop.to_string()])
        // Frame-pipelining depth -- see EncodeSection::async_depth doc comment.
        // Low by design: this is an interactive control loop, not a video export.
        .args(["-async_depth", &cfg.encode.async_depth.to_string()])
        .args(["-f", "h264", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn ffmpeg (is it on PATH? is the vaapi render node correct?)")
}
```

- [ ] **Step 6: Verify VA-API actually entered CBR**

ffmpeg accepting `-rc_mode CBR` does not prove the driver honoured it, and a silent fallback to constant-QP would undo this entire task while looking fine.

Run:
```bash
ffmpeg -hide_banner -loglevel verbose -init_hw_device vaapi=va:/dev/dri/renderD128 \
  -f lavfi -i testsrc=size=1920x1080:rate=30 -frames:v 60 \
  -vf format=nv12,hwupload -c:v h264_vaapi -rc_mode CBR \
  -b:v 8000k -maxrate 8000k -bufsize 800k -bf 0 -g 15 -f h264 /dev/null 2>&1 | grep -i "rc mode\|rate control"
```
Expected: a line naming CBR. If it reports CQP or VBR instead, adjust the flags (some drivers need `-rc_mode 2` numerically, or reject CBR for the chosen profile) until the log confirms CBR, and record what worked in a comment.

- [ ] **Step 7: Commit**

```bash
git add crates/palmtopd/src/modes.rs crates/palmtopd/src/main.rs crates/palmtopd/src/encode.rs
git commit -m "feat(host): quality presets with bitrate-capped encoding"
```

---

### Task 6: Wire modes into the session — SetMode, encoder restart, bounded send buffer

**Files:**
- Modify: `crates/palmtopd/src/session.rs`
- Modify: `crates/palmtopd/Cargo.toml`

**Interfaces:**
- Consumes: `modes::Mode`, `modes::Preset`, `encode::spawn(cfg, preset, src_w, src_h)`, `encode::TimestampFifo`.
- Produces: a session that responds to `Message::SetMode` by restarting the encode stage and re-sending `VideoConfig`, and that answers `Ping` with a timestamped `Pong`.

- [ ] **Step 1: Add socket2**

In `crates/palmtopd/Cargo.toml` under `[dependencies]`:

```toml
socket2 = "0.5"
```

- [ ] **Step 2: Bound the kernel send buffer**

In `handle_client`, after `stream.set_nodelay(true).ok();`, add a helper and call it once the mode is known:

```rust
/// Caps how much encoded video the kernel will hold on our behalf.
///
/// `LatestEncoded` carefully drops stale frames and then hands the survivor to
/// a kernel send buffer that may hold several more -- where nothing can drop
/// them, and where they are invisible unless you go looking. The "never queue"
/// invariant that this pipeline enforces at every other stage simply stopped
/// at the syscall boundary. A full buffer makes `write_all` block, which is
/// the point: that is backpressure, and `LatestEncoded` keeps discarding stale
/// frames for as long as the writer is parked.
fn set_send_buffer(stream: &TcpStream, bytes: usize) {
    use socket2::SockRef;
    if let Err(e) = SockRef::from(stream).set_send_buffer_size(bytes) {
        eprintln!("[net] could not set SO_SNDBUF to {bytes}: {e} (continuing with the default)");
    }
}
```

- [ ] **Step 3: Restructure handle_client around a restartable encode stage**

Replace the body between the capture request and `run_network_reader` so the encode stage can be torn down and rebuilt on a mode change. Extract it into a helper:

```rust
/// Everything that has to be rebuilt when the quality mode changes: the ffmpeg
/// process, the feeder, and the Annex-B reader. Capture and the network
/// threads survive a mode switch untouched.
struct EncodeStage {
    child: std::process::Child,
    feeder: thread::JoinHandle<()>,
    reader: thread::JoinHandle<()>,
}

fn start_encode_stage(
    cfg: &HostConfig,
    preset: &crate::modes::Preset,
    src_width: u32,
    src_height: u32,
    slot: Arc<FrameSlot>,
    latest_encoded: Arc<encode::LatestEncoded>,
    stop: Arc<AtomicBool>,
) -> Result<EncodeStage> {
    let mut child = encode::spawn(cfg, preset, src_width, src_height)?;
    let ffmpeg_stdin = child.stdin.take().context("ffmpeg stdin")?;
    let ffmpeg_stdout = child.stdout.take().context("ffmpeg stdout")?;
    let timestamps = encode::TimestampFifo::new();

    let feeder = {
        let (slot, stop, timestamps) = (slot, stop, timestamps.clone());
        thread::spawn(move || encode::run_feeder(ffmpeg_stdin, slot, timestamps, stop))
    };
    let reader = thread::spawn(move || encode::run_reader(ffmpeg_stdout, latest_encoded, timestamps));
    Ok(EncodeStage { child, feeder, reader })
}

fn stop_encode_stage(stage: EncodeStage, stage_stop: &AtomicBool) {
    stage_stop.store(true, Ordering::Relaxed);
    let _ = stage.feeder.join(); // drops ffmpeg stdin -> ffmpeg flushes and exits
    let mut child = stage.child;
    let _ = child.wait();
    let _ = stage.reader.join(); // stdout EOF once ffmpeg has gone
}
```

Note the encode stage gets its **own** stop flag, separate from the session-wide one, so a mode change stops the encoder without tearing down the session.

- [ ] **Step 4: Handle SetMode in the reader thread**

`run_network_reader` gains a `mode_tx: Sender<crate::modes::Mode>` and a new match arm:

```rust
            Ok(Some(Message::SetMode { mode })) => match crate::modes::Mode::from_u8(mode) {
                Some(m) => {
                    println!("[net] client requested {} mode", m.name());
                    let _ = mode_tx.send(m);
                }
                None => eprintln!("[net] client requested unknown mode {mode} -- ignoring"),
            },
```

The main `handle_client` loop, instead of blocking forever in `run_network_reader`, runs the reader on its own thread and waits on `mode_rx` with a timeout, restarting the encode stage and re-sending `VideoConfig` whenever a mode arrives. Restarting ffmpeg naturally emits an IDR first, so the client always has a keyframe to resync on.

- [ ] **Step 5: Answer Ping with a timestamped Pong**

Replace the `Ping` arm in `run_network_reader`:

```rust
            Ok(Some(Message::Ping { nonce, t_client_us })) => {
                let t_host_recv_us = capture::monotonic_us();
                let _ = pong_tx.send((nonce, t_client_us, t_host_recv_us));
            }
```

and in `run_writer`, stamp the send moment as late as possible so the host-side processing delay the client subtracts is accurate:

```rust
        while let Ok((nonce, t_client_us, t_host_recv_us)) = pong_rx.try_recv() {
            let msg = Message::Pong {
                nonce,
                t_client_us,
                t_host_recv_us,
                // Taken here, immediately before serialising, so that the
                // interval the client removes from its RTT genuinely covers
                // the whole time we held the probe.
                t_host_send_us: capture::monotonic_us(),
            };
            if send_encrypted(&noise, &mut write_half, &msg).is_err() {
                return;
            }
        }
```

The `pong_tx`/`pong_rx` channel type changes from `u64` to `(u64, u64, u64)`.

- [ ] **Step 6: Verify it builds and the daemon still streams**

Run: `cargo build --release -p palmtopd && cargo test --workspace`
Expected: builds, all tests pass.

Run: `./scripts/install-service.sh && journalctl --user -u palmtopd -n 20 --no-pager`
Expected: `[net] listening`, `[input] ready`, no errors.

- [ ] **Step 7: Commit**

```bash
git add crates/palmtopd/Cargo.toml crates/palmtopd/src/session.rs Cargo.lock
git commit -m "feat(host): SetMode handling, encoder restart, bounded send buffer"
```

---

### Task 7: Client — ping loop, latency tracking, age-based frame dropping

**Files:**
- Modify: `android/app/src/main/java/dev/palmtop/spike/MainActivity.java`

**Interfaces:**
- Consumes: `LatencyTracker`, `Protocol.ping(nonce, tClientUs)`, `Received.captureUs`, `Received.tHostRecvUs`, `Received.tHostSendUs`.
- Produces: `MainActivity.latency` (a `LatencyTracker` field), `MainActivity.dropBudgetUs` (a `volatile long`).

- [ ] **Step 1: Add the fields**

```java
    private final LatencyTracker latency = new LatencyTracker();
    /** Frames older than this are skipped. Set from the active mode's preset. */
    private volatile long dropBudgetUs = 80_000; // Balanced default
    private static long nowUs() { return System.nanoTime() / 1000L; }
```

- [ ] **Step 2: Drive the ping loop from the writer thread**

In `runWriter`, send a ping on a schedule — a burst of 5 at 200 ms on connect, then one per second:

```java
    // A burst first: with one probe per second it would take ~15s to fill the
    // offset window, and until it does, every end-to-end figure comes from a
    // barely-sampled offset. Reporting nothing beats reporting that.
    long nextPingAt = 0, pingsSent = 0, nonce = 0;
    // ... inside the writer's loop:
    long now = nowUs();
    if (now >= nextPingAt) {
        enqueue(Protocol.ping(++nonce, now));
        pingsSent++;
        nextPingAt = now + (pingsSent < 5 ? 200_000L : 1_000_000L);
    }
```

- [ ] **Step 3: Handle Pong in the read loop**

Replace the deleted `TAG_PING` branch from Task 2:

```java
                } else if (msg.tag == Protocol.TAG_PONG) {
                    latency.onPong(msg.tClientUs, msg.tHostRecvUs, msg.tHostSendUs, nowUs());
                }
```

- [ ] **Step 4: Replace the availability proxy with an age test**

Replace the `if (!msg.keyframe && rawIn.available() > 0)` block. Keep the existing comment explaining why keyframes are never dropped — it records a real bug — and add why age beats availability:

```java
                    // Age, not byte-availability. `available() > 0` only says
                    // "something else is already buffered locally", which
                    // misses a newer frame still in flight and fires
                    // spuriously when the tail of the current frame is what's
                    // buffered. Now that every frame carries the host's
                    // capture time, staleness can be asked about directly.
                    //
                    // Keyframes are still never dropped, no matter how stale:
                    // they carry SPS/PPS and are the only frames the decoder
                    // can restart from. Dropping one leaves the decoder
                    // consuming P-frames with no reference, producing no
                    // output at all, which starves the inFlight permit
                    // forever. (Found by doing exactly this.)
                    boolean stale = false;
                    if (!msg.keyframe && msg.captureUs != 0 && latency.hasOffset()) {
                        long captureClientUs = msg.captureUs - latency.offsetUs();
                        stale = (nowUs() - captureClientUs) > dropBudgetUs;
                    }
                    if (stale) {
                        droppedFrames++;
                        latency.recordDrop();
                    } else {
                        decodedFrames++;
                        feedDecoder(msg.data, msg.captureUs);
                    }
```

- [ ] **Step 5: Carry the timestamp through MediaCodec**

MediaCodec preserves `presentationTimeUs` through decode, so the capture time can ride through the codec itself rather than needing a side map keyed on frame order.

Change `feedDecoder`:

```java
    private void feedDecoder(byte[] au, long captureUs) {
        try {
            if (!inFlight.tryAcquire(500, TimeUnit.MILLISECONDS)) {
                Log.w(TAG, "decoder saturated -- dropping frame");
                return;
            }
            Integer index = availableInputs.poll(500, TimeUnit.MILLISECONDS);
            if (index == null) {
                inFlight.release();
                return;
            }
            ByteBuffer buf = codec.getInputBuffer(index);
            buf.clear();
            buf.put(au);
            // The host's capture timestamp, passed as the presentation time so
            // MediaCodec hands it straight back at output. It is on the *host's*
            // monotonic clock deliberately: converting to client time here
            // would break MediaCodec's requirement that presentation
            // timestamps increase monotonically, because the clock offset is
            // re-estimated as new probes arrive and can step backwards.
            codec.queueInputBuffer(index, 0, au.length, captureUs, 0);
        } catch (Exception e) {
            Log.e(TAG, "feedDecoder", e);
        }
    }
```

Decode time needs the moment the frame was *queued*, which `presentationTimeUs`
does not carry. No side map is needed: `inFlight` caps frames in flight at
exactly 1 (the Phase 0 change that took decode latency from ~40 ms to 25 ms), so
there is only ever one outstanding frame and a single field suffices. Add
alongside the other fields:

```java
    /** When the single in-flight frame was queued. Safe as one field only
     *  because inFlight caps frames in flight at 1 -- if that cap is ever
     *  raised, this must become a map keyed on presentation timestamp. */
    private volatile long queuedAtUs = 0;
```

Set it in `feedDecoder`, immediately before `queueInputBuffer`:

```java
            queuedAtUs = nowUs();
```

And record both measurements at output, in the `onOutputBufferAvailable` callback:

```java
            @Override
            public void onOutputBufferAvailable(MediaCodec mc, int index, MediaCodec.BufferInfo info) {
                long outUs = nowUs();
                long decodeUs = queuedAtUs == 0 ? 0 : outUs - queuedAtUs;
                inFlight.release();
                if (info.presentationTimeUs != 0 && latency.hasOffset()) {
                    long captureClientUs = info.presentationTimeUs - latency.offsetUs();
                    latency.recordFrame(outUs - captureClientUs, decodeUs);
                }
                mc.releaseOutputBuffer(index, true);
            }
```

- [ ] **Step 6: Build and verify live**

Run: `cd android && JAVA_HOME=$HOME/opt/jdk-17.0.19+10 ./gradlew assembleDebug && adb install -r app/build/outputs/apk/debug/app-debug.apk`
Expected: `BUILD SUCCESSFUL`, `Success`.

Then connect the phone and confirm streaming still works and `adb logcat -s PalmtopClient` shows no new errors.

- [ ] **Step 7: Commit**

```bash
git add android/app/src/main/java/dev/palmtop/spike/MainActivity.java
git commit -m "feat(client): ping loop, latency tracking, age-based frame drop"
```

---

### Task 8: Client — HUD overlay and mode picker

**Files:**
- Create: `android/app/src/main/java/dev/palmtop/spike/HudView.java`
- Modify: `android/app/src/main/java/dev/palmtop/spike/MainActivity.java`

**Interfaces:**
- Consumes: `LatencyTracker.Stats`.
- Produces: `HudView(Context)`, `void update(LatencyTracker.Stats stats, String modeName, int width, int height, int fps)`, `void setShown(boolean)`.

- [ ] **Step 1: Implement HudView**

Create `android/app/src/main/java/dev/palmtop/spike/HudView.java`:

```java
package dev.palmtop.spike;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.view.View;

import java.util.Locale;

/**
 * Live latency overlay. Exists so a mode change can be judged by its effect
 * rather than by how it feels, which is exactly the trap this whole workstream
 * came out of.
 *
 * The end-to-end figure is labelled `~` on purpose. It is capture-to-decoded,
 * derived through a clock offset that assumes symmetric network delay, and it
 * excludes the panel's own response time entirely (unmeasurable without
 * external hardware). Presenting it as a bare number would invite quoting it
 * as if it were glass-to-glass, which it is not.
 */
final class HudView extends View {
    private final Paint text = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint background = new Paint();
    private String[] lines = { "measuring…" };
    private boolean shown = false;

    HudView(Context context) {
        super(context);
        float density = getResources().getDisplayMetrics().density;
        text.setColor(Color.WHITE);
        text.setTextSize(12f * density);
        text.setTypeface(android.graphics.Typeface.MONOSPACE);
        background.setColor(Color.argb(170, 0, 0, 0));
        setVisibility(GONE);
    }

    void setShown(boolean shown) {
        this.shown = shown;
        setVisibility(shown ? VISIBLE : GONE);
    }

    void update(LatencyTracker.Stats s, String modeName, int width, int height, int fps) {
        if (!shown) return;
        lines = s.valid
                ? new String[] {
                    String.format(Locale.US, "~e2e %d/%dms p50/p95", s.e2eP50 / 1000, s.e2eP95 / 1000),
                    String.format(Locale.US, "rtt %dms  dec %dms", s.rttP50 / 1000, s.decodeP50 / 1000),
                    String.format(Locale.US, "drop %.1f%%", s.dropPercent),
                    String.format(Locale.US, "%s  %dx%d@%d", modeName, width, height, fps),
                }
                : new String[] { "measuring…", String.format(Locale.US, "%s  %dx%d@%d", modeName, width, height, fps) };
        postInvalidate();
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        float density = getResources().getDisplayMetrics().density;
        float pad = 6f * density, lineHeight = text.getTextSize() * 1.35f;
        float boxW = 0;
        for (String line : lines) boxW = Math.max(boxW, text.measureText(line));
        canvas.drawRect(0, 0, boxW + pad * 2, lineHeight * lines.length + pad * 2, background);
        float y = pad + text.getTextSize();
        for (String line : lines) {
            canvas.drawText(line, pad, y, text);
            y += lineHeight;
        }
    }
}
```

- [ ] **Step 2: Add the HUD and mode picker to MainActivity**

Add the `HudView` to the root `FrameLayout` (top-right gravity), a `📊` toggle button and a `⚙` mode button to the existing control row beside Reconnect and ⌨. The mode button opens an `AlertDialog` with the four names; selecting one calls:

```java
    private void selectMode(int mode) {
        this.currentMode = mode;
        prefs().edit().putInt("mode", mode).apply();
        // Applied locally right away so frame-age dropping matches the mode
        // even before the host's new VideoConfig lands.
        this.dropBudgetUs = DROP_BUDGET_US[mode];
        enqueue(Protocol.setMode(mode));
    }

    private static final String[] MODE_NAMES = { "Sync", "Balanced", "Quality", "Battery" };
    private static final long[] DROP_BUDGET_US = { 40_000, 80_000, 150_000, 120_000 };
```

Restore the persisted mode on connect and re-send it after the handshake, so a session resumed after an app restart comes back in the chosen mode rather than silently reverting to the host default.

- [ ] **Step 3: Drive HUD updates**

In the existing every-30-frames status update block, also call:

```java
                        hud.update(latency.snapshot(), MODE_NAMES[currentMode], cfg.width, cfg.height, cfg.fps);
```

- [ ] **Step 4: Handle mid-session VideoConfig (decoder reconfigure)**

A mode change with a different resolution means a new `VideoConfig` mid-stream. Because TCP is ordered, **every frame after that message is in the new resolution** — so there is no ambiguity about which frames belong to which configuration, and the client simply rebuilds the decoder before accepting any of them:

```java
                } else if (msg.tag == Protocol.TAG_VIDEO_CONFIG) {
                    if (cfg == null || msg.width != cfg.width || msg.height != cfg.height) {
                        Log.i(TAG, "video config changed to " + msg.width + "x" + msg.height
                                + " -- rebuilding decoder");
                        releaseCodec();
                        configureCodec(surfaceHolder, msg.width, msg.height);
                    }
                    cfg = msg;
                }
```

`releaseCodec()` must stop and release the codec, quit the codec `HandlerThread`, and drain `availableInputs` and the `inFlight` semaphore back to their initial state — a stale permit count would silently throttle the rebuilt decoder to fewer frames in flight than intended.

- [ ] **Step 5: Build and verify each mode switches live**

Run: `cd android && JAVA_HOME=$HOME/opt/jdk-17.0.19+10 ./gradlew assembleDebug && adb install -r app/build/outputs/apk/debug/app-debug.apk`

Then, on the device: connect, toggle the HUD on, and switch through all four modes. Expected: picture continues in every case, HUD resolution line changes for Sync and Battery (720p) versus Balanced and Quality (1080p), no decoder errors in `adb logcat -s PalmtopClient`.

- [ ] **Step 6: Commit**

```bash
git add android/app/src/main/java/dev/palmtop/spike/HudView.java android/app/src/main/java/dev/palmtop/spike/MainActivity.java
git commit -m "feat(client): latency HUD and quality-mode picker"
```

---

### Task 9: Benchmark script and the measurement run

**Files:**
- Create: `scripts/measure-latency.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Measure streaming latency per quality mode.
#
#   ./scripts/measure-latency.sh --content "static terminal"
#   ./scripts/measure-latency.sh --content "1080p video playback" --duration 45
#
# Video latency depends heavily on what is on screen -- a static terminal and a
# playing video are not the same workload, and comparing a run of one against a
# run of the other is meaningless. --content is required rather than optional
# precisely so that trap is hard to fall into: it is recorded in the output, and
# runs are only comparable when it matches.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/scripts/device.sh"

DURATION=30
CONTENT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --duration) DURATION="$2"; shift 2 ;;
    --content)  CONTENT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
[ -n "$CONTENT" ] || { echo "error: --content \"<description>\" is required" >&2; exit 1; }

echo "content : $CONTENT"
echo "device  : $DEVICE_NAME"
echo "duration: ${DURATION}s per mode"
echo
printf '%-10s %8s %8s %8s %8s\n' mode e2e_p50 e2e_p95 rtt_p50 drop%
```

For each of the four modes: switch the phone into it, wait `DURATION`, then read the client's reported stats from logcat and print a row. Add a periodic `Log.i(TAG, "stats ...")` line in `MainActivity`'s HUD-update block emitting machine-readable `key=value` pairs so the script can parse them without screen-scraping the HUD.

- [ ] **Step 2: Make it executable and run the baseline**

```bash
chmod +x scripts/measure-latency.sh
./scripts/measure-latency.sh --content "static terminal"
./scripts/measure-latency.sh --content "1080p video playback"
```

- [ ] **Step 3: Answer the spec's three open questions with the data**

1. Does 720p60 beat 720p30 for Sync on this device, or does it hit the same decode-ceiling wall 1080p60 did? (Temporarily set `Mode::Sync`'s `fps` to 60 and re-run.)
2. Does bounding `SO_SNDBUF` measurably help? (Compare against a build with `set_send_buffer` commented out.)
3. What dominates the residual latency after all of §2? If it is network stall rather than encode or decode, that is the evidence that would justify reconsidering UDP/QUIC.

Record the answers in `README.md` and the plan document. **Report what the numbers actually show, including if a change made no measurable difference** — a fix that did not help is worth knowing about, and quietly keeping it because it seemed like a good idea is how pipelines accumulate cargo cult.

- [ ] **Step 4: Commit**

```bash
git add scripts/measure-latency.sh README.md
git commit -m "feat: per-mode latency benchmark and measured results"
```

---

## Self-Review Notes

**Spec coverage:** §1.1 clock offset → Tasks 1, 3, 6, 7. §1.2 frame timestamps → Tasks 1, 4. §1.3 reporting → Tasks 3, 7. §1.4 surfaces → Tasks 8, 9. §2.1 bitrate cap → Task 5. §2.2 SO_SNDBUF → Tasks 5, 6. §2.3 frame-age drop → Task 7. §2.4 send the pings → Task 7. §3 modes → Tasks 5, 6, 8. §4 structure → Tasks 3, 5, 8. §5 testing → Tasks 1, 3, 4, 5, 9. All covered.

**Deliberately deferred:** the derived input→screen figure from §1.3 is not built. It is arithmetic over numbers Tasks 7 and 9 already produce, and adding a second latency figure before the primary one is validated would mean two unverified numbers instead of one.
