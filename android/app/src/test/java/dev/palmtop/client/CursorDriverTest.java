package dev.palmtop.client;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/**
 * The joystick's motion maths, tested without an Android device.
 *
 * Deltas are in *host desktop pixels* -- PointerMotionRelative reaches the
 * compositor verbatim as f64 logical pixels (palmtopd/src/input.rs), so these
 * numbers are what the laptop's cursor actually moves.
 */
public class CursorDriverTest {

    private static final float EPS = 0.5f;

    /** Magnitude of a delta pair. */
    private static float mag(float[] d) {
        return (float) Math.hypot(d[0], d[1]);
    }

    @Test
    public void restProducesNoMotion() {
        float[] d = CursorDriver.deltaFor(0f, 0f, 16L);
        assertEquals(0f, d[0], 0f);
        assertEquals(0f, d[1], 0f);
    }

    @Test
    public void insideDeadzoneProducesNoMotion() {
        // A thumb resting on the pad must not drift the cursor.
        float[] d = CursorDriver.deltaFor(0.10f, 0f, 16L);
        assertEquals(0f, d[0], 0f);
        assertEquals(0f, d[1], 0f);
    }

    /**
     * The longest span a single tick may be billed for. Speed assertions are
     * measured over exactly this: anything longer is clamped by design (see
     * {@link #elapsedTimeIsClampedSoAStallCannotTeleportTheCursor}), so a
     * "distance travelled in one second" assertion could never hold and would
     * be testing the clamp rather than the speed.
     */
    private static final long FULL_TICK = CursorDriver.MAX_TICK_MS;

    /** Distance covered at {@code fraction} of max speed over FULL_TICK. */
    private static float expectedOverFullTick(float fraction) {
        return fraction * CursorDriver.MAX_SPEED_PX_S * (FULL_TICK / 1000f);
    }

    @Test
    public void fullDeflectionTravelsAtMaxSpeed() {
        float[] d = CursorDriver.deltaFor(1f, 0f, FULL_TICK);
        assertEquals(expectedOverFullTick(1f), d[0], EPS);
        assertEquals(0f, d[1], EPS);
    }

    @Test
    public void speedIsContinuousAcrossTheDeadzoneEdge() {
        // Just outside the deadzone must be near-zero, not a jump to some
        // fraction of full speed -- otherwise the cursor lurches the instant
        // the thumb crosses the threshold.
        float[] d = CursorDriver.deltaFor(CursorDriver.DEADZONE + 0.001f, 0f, FULL_TICK);
        assertTrue("expected near-zero, got " + d[0], Math.abs(d[0]) < 1f);
    }

    @Test
    public void responseCurveIsQuadraticAfterRescaling() {
        // raw 0.5 -> rescaled m = (0.5-0.12)/(1-0.12) = 0.4318
        // m^2 = 0.1865 -> markedly less than half speed, which is the point:
        // fine control near centre, full speed still reachable at the rim.
        float[] d = CursorDriver.deltaFor(0.5f, 0f, FULL_TICK);
        assertEquals(expectedOverFullTick(0.1865f), d[0], EPS);
    }

    @Test
    public void deltaScalesWithElapsedTime() {
        float[] one = CursorDriver.deltaFor(1f, 0f, 16L);
        float[] two = CursorDriver.deltaFor(1f, 0f, 32L);
        assertEquals(one[0] * 2f, two[0], EPS);
    }

    @Test
    public void diagonalIsNotFasterThanCardinal() {
        // The classic joystick bug: unclamped diagonals travel sqrt(2) times
        // faster than a cardinal push at the same deflection.
        float[] diagonal = CursorDriver.deltaFor(1f, 1f, FULL_TICK);
        assertEquals(expectedOverFullTick(1f), mag(diagonal), EPS);
    }

    @Test
    public void directionIsPreserved() {
        float[] d = CursorDriver.deltaFor(-0.8f, 0.6f, FULL_TICK);
        assertTrue("x should be negative, got " + d[0], d[0] < 0f);
        assertTrue("y should be positive, got " + d[1], d[1] > 0f);
        // 3-4-5 triangle: the -0.8/0.6 ratio must survive the curve.
        assertEquals(-4f / 3f, d[0] / d[1], 0.01f);
    }

    @Test
    public void elapsedTimeIsClampedSoAStallCannotTeleportTheCursor() {
        float[] stalled = CursorDriver.deltaFor(1f, 0f, 5000L);
        float[] clamped = CursorDriver.deltaFor(1f, 0f, CursorDriver.MAX_TICK_MS);
        assertEquals(clamped[0], stalled[0], EPS);
    }
}
