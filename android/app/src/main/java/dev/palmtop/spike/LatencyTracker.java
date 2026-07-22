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
 * arithmetic that can be wrong by a sign or a factor of two while still
 * producing numbers that look like plausible latencies on a running device. A
 * plausible wrong number is worse than no number, because it gets quoted.
 *
 * <h3>The symmetry assumption, stated plainly</h3>
 * The offset estimate assumes network delay is the same in both directions.
 * Real asymmetry {@code A} produces an offset error of {@code A/2}, and that
 * error flows straight into every end-to-end figure derived from it. Two things
 * reduce it and neither eliminates it: a window of probes is kept and the one
 * with the <em>lowest</em> RTT is used (least queuing in either direction,
 * therefore least asymmetry), and LAN round trips are short to begin with.
 *
 * So: treat the resulting end-to-end number as an estimate carrying a few
 * milliseconds of uncertainty. It is not a measurement and must not be
 * presented as one -- the same standard already applied to the display leg,
 * which no software on this device can measure at all.
 */
public final class LatencyTracker {
    private static final int OFFSET_WINDOW = 16;
    /** ~8s of frames at 30fps -- long enough for a stable p95, short enough to track changes. */
    private static final int SAMPLE_WINDOW = 240;

    /** One round-trip probe: the offset it implies, and how far to trust it. */
    private static final class Probe {
        final long offsetUs;
        final long rttUs;

        Probe(long offsetUs, long rttUs) {
            this.offsetUs = offsetUs;
            this.rttUs = rttUs;
        }
    }

    private final Deque<Probe> probes = new ArrayDeque<>();
    private final Deque<Long> e2eSamples = new ArrayDeque<>();
    private final Deque<Long> decodeSamples = new ArrayDeque<>();
    private long framesDecoded = 0;
    private long framesDropped = 0;

    /**
     * Records one completed Ping/Pong round trip.
     *
     * @param tClientSendUs client monotonic clock when the Ping went out
     * @param tHostRecvUs   host monotonic clock when it arrived
     * @param tHostSendUs   host monotonic clock when the Pong went out
     * @param tClientRecvUs client monotonic clock when the Pong came back
     */
    public synchronized void onPong(long tClientSendUs, long tHostRecvUs,
                                    long tHostSendUs, long tClientRecvUs) {
        // Round trip minus however long the host held the probe, so host-side
        // scheduling delay never inflates the network figure.
        long rtt = (tClientRecvUs - tClientSendUs) - (tHostSendUs - tHostRecvUs);
        long offset = ((tHostRecvUs - tClientSendUs) + (tHostSendUs - tClientRecvUs)) / 2;
        if (rtt < 0) {
            // Physically impossible: a reordered reply, or a clock that moved
            // backwards. Silently dropping it is right -- one corrupt probe in
            // the window could win the min-RTT selection and skew everything.
            return;
        }
        probes.addLast(new Probe(offset, rtt));
        while (probes.size() > OFFSET_WINDOW) {
            probes.removeFirst();
        }
    }

    public synchronized boolean hasOffset() {
        return !probes.isEmpty();
    }

    /**
     * Host clock minus client clock. Convert a host timestamp to client time
     * with {@code hostUs - offsetUs()}.
     *
     * Returns the offset from the lowest-RTT probe rather than an average --
     * see the class comment on the symmetry assumption.
     */
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
        while (q.size() > SAMPLE_WINDOW) {
            q.removeFirst();
        }
    }

    public static final class Stats {
        public long e2eP50, e2eP95, rttP50, decodeP50;
        public double dropPercent;
        /** False until there is both a clock offset and at least one decoded frame. */
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
        s.valid = !probes.isEmpty() && !e2eSamples.isEmpty();
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
