package dev.palmtop.client;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

/**
 * The joystick pad's geometry, tested as pure maths -- no Android view is
 * instantiated, which is what lets these run on the JVM.
 */
public class JoystickViewTest {

    private static final float EPS = 0.001f;
    private static final float CX = 100f, CY = 100f, R = 50f;

    @Test
    public void centreIsZero() {
        float[] v = JoystickView.vectorFor(CX, CY, CX, CY, R);
        assertEquals(0f, v[0], EPS);
        assertEquals(0f, v[1], EPS);
    }

    @Test
    public void rimRightIsPositiveX() {
        float[] v = JoystickView.vectorFor(CX + R, CY, CX, CY, R);
        assertEquals(1f, v[0], EPS);
        assertEquals(0f, v[1], EPS);
    }

    @Test
    public void rimLeftIsNegativeX() {
        float[] v = JoystickView.vectorFor(CX - R, CY, CX, CY, R);
        assertEquals(-1f, v[0], EPS);
        assertEquals(0f, v[1], EPS);
    }

    @Test
    public void downIsPositiveY() {
        // Screen convention, matching MotionEvent and the host's own axes:
        // pushing the stick down must move the cursor down, not up.
        float[] v = JoystickView.vectorFor(CX, CY + R, CX, CY, R);
        assertEquals(0f, v[0], EPS);
        assertEquals(1f, v[1], EPS);
    }

    @Test
    public void upIsNegativeY() {
        float[] v = JoystickView.vectorFor(CX, CY - R, CX, CY, R);
        assertEquals(-1f, v[1], EPS);
    }

    @Test
    public void halfwayIsHalfMagnitude() {
        float[] v = JoystickView.vectorFor(CX + R / 2f, CY, CX, CY, R);
        assertEquals(0.5f, v[0], EPS);
    }

    @Test
    public void beyondTheRimClampsToUnitMagnitude() {
        // A thumb sliding off the pad must saturate, not report magnitude 4.
        float[] v = JoystickView.vectorFor(CX + R * 4f, CY, CX, CY, R);
        assertEquals(1f, v[0], EPS);
    }

    @Test
    public void clampingPreservesDirection() {
        // Far out on a 3-4-5 diagonal: magnitude clamps to 1, ratio survives.
        float[] v = JoystickView.vectorFor(CX + 300f, CY + 400f, CX, CY, R);
        assertEquals(1f, (float) Math.hypot(v[0], v[1]), EPS);
        assertEquals(0.6f, v[0], EPS);
        assertEquals(0.8f, v[1], EPS);
    }

    @Test
    public void zeroRadiusDoesNotDivideByZero() {
        // Defensive: vectorFor can be called before the view has been laid
        // out, when its radius is still 0.
        float[] v = JoystickView.vectorFor(CX + 10f, CY, CX, CY, 0f);
        assertEquals(0f, v[0], EPS);
        assertEquals(0f, v[1], EPS);
    }
}
