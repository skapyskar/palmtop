package dev.palmtop.client;

import android.os.Handler;
import android.os.Looper;

/**
 * Turns the joystick's stick position into a stream of relative cursor
 * movements.
 *
 * <p>Split from {@link JoystickView} on purpose. The view's job is to be a
 * stick -- draw a pad, follow a thumb, report where it is pushed. This class
 * owns the question the view has no business answering: given the stick is
 * pushed <em>there</em>, how far should the laptop's cursor actually travel?
 * Keeping that here means the maths is a pure function of (vector, elapsed
 * time) and can be tested without an Android device attached, which is the
 * only way any of it gets verified in this project's environment.
 *
 * <h3>Units</h3>
 * Deltas are in <b>host desktop pixels</b>, not phone pixels.
 * {@code PointerMotionRelative} is handed to the compositor verbatim as f64
 * logical pixels (see palmtopd's input.rs), so the numbers here are literally
 * how far the laptop's cursor moves. Phone density is irrelevant.
 *
 * <h3>Why the curve</h3>
 * A linear stick is useless for the thing a joystick is actually for here:
 * nudging a window edge or placing a text caret. Squaring the deflection buys
 * fine control across most of the pad's travel while still reaching full speed
 * at the rim, so one control covers both "move two pixels" and "cross the
 * screen".
 */
final class CursorDriver {

    /** Raw stick magnitude below which nothing moves. A thumb resting on the
     *  pad is never exactly centred, and without this the cursor drifts
     *  continuously. */
    static final float DEADZONE = 0.12f;

    /** Host pixels per second at full deflection, at the slowest and fastest
     *  ends of the sensitivity slider. The usable range was chosen so that
     *  the slow end is genuinely usable for pixel work rather than merely
     *  "less fast", and the fast end still crosses a 1080p desktop in about
     *  a second. */
    static final float MIN_SPEED_PX_S = 200f;
    static final float MAX_SPEED_PX_S = 1600f;

    /** Where the slider starts. Deliberately well below the original fixed
     *  1200: that first value was reasoned about rather than felt, and the
     *  first person to actually use it found it too fast. */
    static final float DEFAULT_SPEED_PX_S = 700f;

    /** Motion tick, ~60Hz. */
    static final long TICK_MS = 16L;

    /** Upper bound on the elapsed time any single tick may account for. The
     *  tick runs on the UI looper, so a slow frame -- or the activity being
     *  briefly starved -- can leave a long gap since the last one. Billing
     *  that whole gap would jump the cursor across the screen in one step.
     *  Better to under-travel slightly than to teleport. */
    static final long MAX_TICK_MS = 100L;

    /** Where movements go. Kept as an interface so this class never learns
     *  about sockets, framing or the session. */
    interface Sink {
        void move(float dx, float dy);
    }

    private final Sink sink;
    private final Handler handler = new Handler(Looper.getMainLooper());

    private float vectorX, vectorY;
    private boolean ticking;
    private long lastTickAt;
    /** Live, user-adjustable -- see {@link #setMaxSpeed}. */
    private float maxSpeedPxS = DEFAULT_SPEED_PX_S;

    private final Runnable tick = this::onTick;

    CursorDriver(Sink sink) {
        this.sink = sink;
    }

    /** Sets the speed at full deflection, clamped to the supported range.
     *  Takes effect on the next tick, so it can be changed mid-push. */
    void setMaxSpeed(float pxPerSecond) {
        maxSpeedPxS = clampSpeed(pxPerSecond);
    }

    float maxSpeed() {
        return maxSpeedPxS;
    }

    static float clampSpeed(float pxPerSecond) {
        if (pxPerSecond < MIN_SPEED_PX_S) return MIN_SPEED_PX_S;
        if (pxPerSecond > MAX_SPEED_PX_S) return MAX_SPEED_PX_S;
        return pxPerSecond;
    }

    /**
     * How far the cursor travels for a stick pushed to (vx, vy) over
     * {@code elapsedMs}. Pure -- no state, no side effects, no Android.
     *
     * @param vx stick x, -1..1
     * @param vy stick y, -1..1 (screen convention: down is positive)
     * @param elapsedMs real time since the previous tick
     * @param maxSpeedPxS speed at full deflection, in host pixels per second
     * @return {dx, dy} in host desktop pixels
     */
    static float[] deltaFor(float vx, float vy, long elapsedMs, float maxSpeedPxS) {
        float raw = (float) Math.hypot(vx, vy);
        if (raw < DEADZONE) return new float[] { 0f, 0f };

        // Rescale so speed is continuous across the deadzone edge rather than
        // jumping to some fraction of full speed the instant it is crossed --
        // and so full deflection still reaches exactly full speed.
        float m = (raw - DEADZONE) / (1f - DEADZONE);
        if (m > 1f) m = 1f;

        // Squared response: fine near centre, full speed at the rim.
        float speed = m * m * maxSpeedPxS;

        long dt = Math.min(elapsedMs, MAX_TICK_MS);
        float distance = speed * (dt / 1000f);

        // Divide by the *raw* magnitude to get a unit direction. Doing this
        // rather than using vx/vy directly is what stops a diagonal push
        // travelling sqrt(2) times faster than a cardinal one at the same
        // deflection -- the classic joystick bug.
        return new float[] { vx / raw * distance, vy / raw * distance };
    }

    /**
     * Reports the stick's current position. Starts the motion tick when the
     * stick leaves centre and stops it when the stick returns, so an idle
     * joystick costs nothing at all.
     */
    void setVector(float vx, float vy) {
        vectorX = vx;
        vectorY = vy;
        boolean active = Math.hypot(vx, vy) >= DEADZONE;
        if (active && !ticking) {
            ticking = true;
            lastTickAt = System.nanoTime() / 1_000_000L;
            handler.postDelayed(tick, TICK_MS);
        } else if (!active && ticking) {
            stop();
        }
    }

    /** Halts ticking. Safe to call when already stopped. */
    void stop() {
        ticking = false;
        handler.removeCallbacks(tick);
    }

    private void onTick() {
        if (!ticking) return;
        long now = System.nanoTime() / 1_000_000L;
        long elapsed = now - lastTickAt;
        lastTickAt = now;

        float[] d = deltaFor(vectorX, vectorY, elapsed, maxSpeedPxS);
        if (d[0] != 0f || d[1] != 0f) {
            sink.move(d[0], d[1]);
        }
        handler.postDelayed(tick, TICK_MS);
    }
}
