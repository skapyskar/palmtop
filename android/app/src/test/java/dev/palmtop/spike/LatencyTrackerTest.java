package dev.palmtop.spike;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/**
 * The clock-offset formula is standard NTP, and its failure mode is quiet: a
 * sign error or a factor of two still yields a number that looks like a
 * latency. These tests construct round trips with a *known* true offset and
 * deliberately asymmetric delays, which is the only way to catch that.
 */
public class LatencyTrackerTest {

    /**
     * Simulates one full round trip and feeds the result to the tracker.
     *
     * @param hostAhead the true clock offset (host clock = client clock + hostAhead)
     */
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
    public void offsetWorksWhenTheHostClockIsBehindTheClient() {
        LatencyTracker t = new LatencyTracker();
        exchange(t, 1_000, 4_000, 100, 4_000, -2_500_000);
        assertEquals(-2_500_000, t.offsetUs());
    }

    @Test
    public void rttExcludesHostProcessingTime() {
        LatencyTracker t = new LatencyTracker();
        exchange(t, 1_000, 5_000, 50_000, 5_000, 0);
        // 5ms up + 5ms down = 10ms, regardless of the 50ms the host held it.
        assertEquals(10_000, t.snapshot().rttP50);
    }

    /**
     * The whole reason for min-RTT selection. A badly asymmetric round trip
     * skews the offset by half the asymmetry; averaging every sample would bake
     * that error in permanently, so the tracker must prefer the least-delayed
     * probe instead.
     */
    @Test
    public void prefersTheLeastDelayedSampleOverASkewedOne() {
        LatencyTracker t = new LatencyTracker();
        // Badly asymmetric: 60ms up, 2ms down -> implies an offset ~29ms wrong.
        exchange(t, 1_000, 60_000, 100, 2_000, 1_000_000);
        // Clean, symmetric, lower RTT -- this is the one to trust.
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

    @Test
    public void statsAreNotValidBeforeAnyMeasurement() {
        LatencyTracker t = new LatencyTracker();
        assertFalse(t.snapshot().valid);
        exchange(t, 1_000, 5_000, 100, 5_000, 0);
        // An offset alone isn't enough -- there must be frames too.
        assertFalse(t.snapshot().valid);
        t.recordFrame(30_000, 10_000);
        assertTrue(t.snapshot().valid);
    }

    /** A reply that implies a negative RTT is nonsense and must not pollute the window. */
    @Test
    public void impossibleRoundTripIsIgnored() {
        LatencyTracker t = new LatencyTracker();
        t.onPong(1_000, 500, 900, 1_200); // host held it longer than the whole trip
        assertFalse(t.hasOffset());
    }
}
