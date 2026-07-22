package dev.palmtop.spike;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

/**
 * Zoom/pan math has the same failure profile as VideoFit's: a wrong clamp or
 * an inverted delta produces a plausible-looking number (the picture just
 * zooms a bit oddly, or lets you pan slightly too far) rather than crashing,
 * so it needs checking against hand-derived cases, not eyeballing on a phone.
 */
public class PinchZoomMathTest {
    private static final float EPS = 0.001f;

    @Test
    public void doublingSpreadDoublesScale() {
        PinchZoomMath z = new PinchZoomMath();
        z.beginSegment(100, 50, 50);
        z.update(200, 50, 50, 1000, 1000, 1000, 1000);
        assertEquals(2.0f, z.getScale(), EPS);
    }

    @Test
    public void scaleNeverExceedsMax() {
        PinchZoomMath z = new PinchZoomMath();
        z.beginSegment(100, 50, 50);
        z.update(1000, 50, 50, 1000, 1000, 1000, 1000); // implies 10x
        assertEquals(PinchZoomMath.MAX_SCALE, z.getScale(), EPS);
    }

    @Test
    public void scaleNeverGoesBelowMin() {
        PinchZoomMath z = new PinchZoomMath();
        z.beginSegment(100, 50, 50);
        z.update(10, 50, 50, 1000, 1000, 1000, 1000); // implies 0.1x
        assertEquals(PinchZoomMath.MIN_SCALE, z.getScale(), EPS);
    }

    @Test
    public void panIsDisallowedAtBaseScaleWithNoCropMargin() {
        // content == visible (no baked-in crop, no interactive zoom yet) --
        // there is nothing extra to reveal, so any attempted pan must clamp
        // to exactly zero, not merely "small".
        PinchZoomMath z = new PinchZoomMath();
        z.beginSegment(100, 0, 0);
        z.update(100, 1000, 0, 1000, 1000, 1000, 1000);
        assertEquals(0f, z.getPanX(), EPS);
    }

    @Test
    public void panIsAllowedUpToTheBakedInCropMargin() {
        // content=1200 vs visible=1000 at scale 1.0 -- exactly the situation
        // a cropped aspect mode produces even with no interactive zoom.
        // maxPanX = (1.0*1200 - 1000)/2 = 100.
        PinchZoomMath z = new PinchZoomMath();
        z.beginSegment(100, 0, 0);
        z.update(100, 50, 0, 1200, 1000, 1000, 1000);
        assertEquals(50f, z.getPanX(), EPS);

        z.update(100, 500, 0, 1200, 1000, 1000, 1000);
        assertEquals(100f, z.getPanX(), EPS); // clamped at the margin
    }

    @Test
    public void resetReturnsToIdentity() {
        PinchZoomMath z = new PinchZoomMath();
        z.beginSegment(100, 0, 0);
        z.update(300, 200, 0, 1000, 1000, 1000, 1000);
        z.endSegment();
        z.reset();
        assertEquals(1.0f, z.getScale(), EPS);
        assertEquals(0f, z.getPanX(), EPS);
        assertEquals(0f, z.getPanY(), EPS);
    }

    /** The whole reason segments exist: re-baselining (a finger joining or
     * leaving mid-gesture) must not lose accumulated zoom. */
    @Test
    public void reBaseliningContinuesFromThePersistedBaseRatherThanResetting() {
        PinchZoomMath z = new PinchZoomMath();
        z.beginSegment(100, 0, 0);
        z.update(200, 0, 0, 1000, 1000, 1000, 1000); // 2x
        z.endSegment(); // base is now 2x

        z.beginSegment(50, 0, 0); // a finger rejoined at a different spacing
        z.update(100, 0, 0, 1000, 1000, 1000, 1000); // another implied 2x
        assertEquals(4.0f, z.getScale(), EPS); // continues from 2x, not from 1x
    }

    @Test
    public void zeroReferenceDistanceDoesNotDivideByZero() {
        PinchZoomMath z = new PinchZoomMath();
        z.beginSegment(0, 0, 0);
        z.update(50, 0, 0, 1000, 1000, 1000, 1000);
        assertEquals(1.0f, z.getScale(), EPS); // no scale change, not NaN/Infinity
    }
}
