# Touch Controls and Settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a top edge inset, move set-once controls behind a Settings sheet, and add a cursor joystick with L/R click buttons to the Palmtop Android client.

**Architecture:** Three independent changes to the Android client only. The joystick splits into `JoystickView` (a custom View producing a normalized vector) and `CursorDriver` (converting that vector into timed relative-motion messages) so the real logic is unit-testable without a device. The Settings sheet reuses the existing `Ui.sheet` overlay pattern. All three ride existing wire messages — no protocol or host changes.

**Tech Stack:** Java (no Kotlin, no XML layouts — UI is built programmatically), Android SDK, Gradle 9.6.1 / AGP 9.3.0, JDK 17, JUnit 4 for JVM unit tests.

## Global Constraints

- **The video's width and aspect ratio must not change.** The control column keeps its existing fixed width of `170 * density` px. `VideoFit` and `resizeSurfaceToFit` geometry are not modified.
- **Absolute tap-to-click on the video is unchanged.** `onTouch` and `mapToVideoFraction` in `MainActivity.java` are not modified. The joystick is purely additive.
- **No protocol change.** `Protocol.VERSION` stays 5. No new tags. Uses only existing `pointerMotionRelative()` and `pointerButton()`.
- **No host / Rust changes.** This is an APK-only change.
- **UI is built programmatically in Java.** No XML layouts. Use the `Ui.*` factories (`Ui.button`, `Ui.iconButton`, `Ui.sheet`, `Ui.mono`, `Ui.dp`, `Ui.sm/md/lg/xl`, `Ui.stacked`) and the `Ui.*` colour tokens (`Ui.PANEL`, `Ui.RAISED`, `Ui.ACCENT`, `Ui.TEXT`, `Ui.TEXT_MUTED`, `Ui.HAIRLINE`) rather than raw colours or new drawables.
- **Build environment:** `JAVA_HOME=/home/skapyskar/opt/jdk-17.0.19+10`, and gradle must be run with `--offline`.
- **Java package:** `dev.palmtop.client`. Source root `android/app/src/main/java/dev/palmtop/client/`, tests `android/app/src/test/java/dev/palmtop/client/`.
- **Commit messages:** authored in the user's name only. Do **not** add a `Co-Authored-By` trailer.
- **Relative motion units are host desktop pixels.** `PointerMotionRelative { dx, dy }` reaches the compositor verbatim as `f64` logical pixels (`crates/palmtopd/src/input.rs:86`).

## Tuning constants (single source of truth)

Defined in `CursorDriver` in Task 2; every later reference uses these names.

| Constant | Value | Meaning |
|---|---|---|
| `DEADZONE` | `0.12f` | raw magnitude below this = no motion |
| `MAX_SPEED_PX_S` | `1200f` | host px/sec at full deflection |
| `TICK_MS` | `16L` | ~60 Hz motion tick |
| `MAX_TICK_MS` | `100L` | elapsed-time clamp, so a stalled UI thread cannot teleport the cursor |

## File Structure

**Create:**
- `android/app/src/main/java/dev/palmtop/client/JoystickView.java` — custom View: draws pad + nub, turns touch into a normalized vector. Knows nothing about the network. Contains the static pure function `vectorFor(...)`.
- `android/app/src/main/java/dev/palmtop/client/CursorDriver.java` — turns a normalized vector into timed relative-motion deltas. Contains the static pure function `deltaFor(...)`. Owns the `Handler` tick.
- `android/app/src/test/java/dev/palmtop/client/JoystickViewTest.java`
- `android/app/src/test/java/dev/palmtop/client/CursorDriverTest.java`

**Modify:**
- `android/app/src/main/java/dev/palmtop/client/MainActivity.java` — top inset (Task 1), settings sheet + rebuilt left column (Task 4), joystick wiring + stuck-button release (Task 5).

**Task order rationale:** Task 1 is independent and lands the smallest visible win first. Tasks 2 and 3 build the joystick's two halves bottom-up with tests, touching no existing code. Task 4 restructures the UI. Task 5 wires the joystick in, and needs Tasks 2, 3 and 4 done.

---

### Task 1: Top edge inset

**Files:**
- Modify: `android/app/src/main/java/dev/palmtop/client/MainActivity.java` (in `buildUi()`, around line 254-295)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other tasks depend on.

**Why the parent, not the video container:** parent padding reduces the child's allocated size, so `videoContainer.getHeight()` already reflects the inset and `resizeSurfaceToFit` re-fits correctly with no change to its logic. Padding `videoContainer` itself would NOT work — it reads `getWidth()`/`getHeight()` including its own padding and centers `videoClip` in the full bounds, so the inset would be silently ignored.

- [ ] **Step 1: Add the top padding to `mainLayout`**

In `buildUi()`, find these lines:

```java
        LinearLayout mainLayout = new LinearLayout(this);
        mainLayout.setOrientation(LinearLayout.HORIZONTAL);
        root.addView(mainLayout, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
```

Replace with:

```java
        LinearLayout mainLayout = new LinearLayout(this);
        mainLayout.setOrientation(LinearLayout.HORIZONTAL);
        // A small breathing space above everything. Without the title bar
        // (removed in v0.3.0) the video sat flush against the phone's top
        // edge, which made the laptop's own top-corner UI -- window close
        // buttons, menu bars -- awkward to hit and easy to lose to the
        // system's edge gestures.
        //
        // Applied to this parent rather than to videoContainer deliberately:
        // parent padding reduces the child's allocated size, so
        // videoContainer.getHeight() already reflects the inset and
        // resizeSurfaceToFit re-fits correctly with no change to its logic.
        // Padding videoContainer itself would be silently ignored -- it
        // measures getWidth()/getHeight() *including* its own padding and
        // centers videoClip within the full bounds. Insetting here also
        // fixes the same reachability problem for the column's top button.
        //
        // Costs the video 12dp of height; its aspect ratio is untouched
        // (VideoFit letterboxes as always) and its width is unchanged.
        mainLayout.setPadding(0, Ui.md(this), 0, 0);
        root.addView(mainLayout, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
```

- [ ] **Step 2: Verify it compiles**

Run:
```bash
cd /home/skapyskar/palmtop/android && JAVA_HOME=/home/skapyskar/opt/jdk-17.0.19+10 ./gradlew --offline compileDebugJavaWithJavac
```
Expected: `BUILD SUCCESSFUL`

- [ ] **Step 3: Commit**

```bash
cd /home/skapyskar/palmtop
git add android/app/src/main/java/dev/palmtop/client/MainActivity.java
git commit -m "feat(android): inset the layout 12dp from the top edge

Without the title bar the video sat flush against the phone's top edge,
making the laptop's top-corner UI awkward to hit. Applied to mainLayout
rather than videoContainer: parent padding reduces the child's allocated
size so resizeSurfaceToFit re-fits correctly, whereas videoContainer
measures its own padding into getHeight() and would ignore it."
```

---

### Task 2: CursorDriver — vector to host-pixel deltas

**Files:**
- Create: `android/app/src/main/java/dev/palmtop/client/CursorDriver.java`
- Test: `android/app/src/test/java/dev/palmtop/client/CursorDriverTest.java`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `static float[] CursorDriver.deltaFor(float vx, float vy, long elapsedMs)` → `{dx, dy}` in host pixels. Pure; the unit under test.
  - `interface CursorDriver.Sink { void move(float dx, float dy); }`
  - `CursorDriver(Sink sink)` constructor
  - `void setVector(float vx, float vy)` — called by `JoystickView`'s listener
  - `void stop()` — halts ticking, for activity teardown
  - `static final float DEADZONE`, `MAX_SPEED_PX_S`; `static final long TICK_MS`, `MAX_TICK_MS`

- [ ] **Step 1: Write the failing test**

Create `android/app/src/test/java/dev/palmtop/client/CursorDriverTest.java`:

```java
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

    @Test
    public void fullDeflectionForOneSecondTravelsMaxSpeed() {
        float[] d = CursorDriver.deltaFor(1f, 0f, 1000L);
        assertEquals(CursorDriver.MAX_SPEED_PX_S, d[0], EPS);
        assertEquals(0f, d[1], EPS);
    }

    @Test
    public void speedIsContinuousAcrossTheDeadzoneEdge() {
        // Just outside the deadzone must be near-zero, not a jump to some
        // fraction of full speed -- otherwise the cursor lurches the instant
        // the thumb crosses the threshold.
        float[] d = CursorDriver.deltaFor(CursorDriver.DEADZONE + 0.001f, 0f, 1000L);
        assertTrue("expected near-zero, got " + d[0], Math.abs(d[0]) < 1f);
    }

    @Test
    public void responseCurveIsQuadraticAfterRescaling() {
        // raw 0.5 -> rescaled m = (0.5-0.12)/(1-0.12) = 0.4318
        // m^2 = 0.1865 -> markedly less than half speed, which is the point:
        // fine control near centre, full speed still reachable at the rim.
        float[] d = CursorDriver.deltaFor(0.5f, 0f, 1000L);
        float expected = 0.1865f * CursorDriver.MAX_SPEED_PX_S;
        assertEquals(expected, d[0], 5f);
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
        float[] diagonal = CursorDriver.deltaFor(1f, 1f, 1000L);
        assertEquals(CursorDriver.MAX_SPEED_PX_S, mag(diagonal), EPS);
    }

    @Test
    public void directionIsPreserved() {
        float[] d = CursorDriver.deltaFor(-0.8f, 0.6f, 1000L);
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
cd /home/skapyskar/palmtop/android && JAVA_HOME=/home/skapyskar/opt/jdk-17.0.19+10 ./gradlew --offline testDebugUnitTest --tests '*CursorDriverTest*'
```
Expected: FAIL — compilation error, `cannot find symbol: class CursorDriver`.

- [ ] **Step 3: Write the implementation**

Create `android/app/src/main/java/dev/palmtop/client/CursorDriver.java`:

```java
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

    /** Host pixels per second at full deflection. */
    static final float MAX_SPEED_PX_S = 1200f;

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

    private final Runnable tick = this::onTick;

    CursorDriver(Sink sink) {
        this.sink = sink;
    }

    /**
     * How far the cursor travels for a stick pushed to (vx, vy) over
     * {@code elapsedMs}. Pure -- no state, no side effects, no Android.
     *
     * @param vx stick x, -1..1
     * @param vy stick y, -1..1 (screen convention: down is positive)
     * @param elapsedMs real time since the previous tick
     * @return {dx, dy} in host desktop pixels
     */
    static float[] deltaFor(float vx, float vy, long elapsedMs) {
        float raw = (float) Math.hypot(vx, vy);
        if (raw < DEADZONE) return new float[] { 0f, 0f };

        // Rescale so speed is continuous across the deadzone edge rather than
        // jumping to some fraction of full speed the instant it is crossed --
        // and so full deflection still reaches exactly full speed.
        float m = (raw - DEADZONE) / (1f - DEADZONE);
        if (m > 1f) m = 1f;

        // Squared response: fine near centre, full speed at the rim.
        float speed = m * m * MAX_SPEED_PX_S;

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

        float[] d = deltaFor(vectorX, vectorY, elapsed);
        if (d[0] != 0f || d[1] != 0f) {
            sink.move(d[0], d[1]);
        }
        handler.postDelayed(tick, TICK_MS);
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run:
```bash
cd /home/skapyskar/palmtop/android && JAVA_HOME=/home/skapyskar/opt/jdk-17.0.19+10 ./gradlew --offline testDebugUnitTest --tests '*CursorDriverTest*'
```
Expected: `BUILD SUCCESSFUL`, 9 tests passing.

- [ ] **Step 5: Commit**

```bash
cd /home/skapyskar/palmtop
git add android/app/src/main/java/dev/palmtop/client/CursorDriver.java android/app/src/test/java/dev/palmtop/client/CursorDriverTest.java
git commit -m "feat(android): add CursorDriver, the joystick's motion maths

Turns a stick position into relative cursor movement in host desktop
pixels (PointerMotionRelative reaches the compositor verbatim as f64
logical pixels, so phone density is irrelevant).

Deadzone stops a resting thumb drifting the cursor, and the magnitude
is rescaled after it so speed is continuous across the threshold rather
than lurching. The squared response curve is what makes one control
cover both 'nudge two pixels' and 'cross the screen'. Direction comes
from dividing by the raw magnitude, so a diagonal push is not sqrt(2)
faster than a cardinal one.

Kept as a pure function of (vector, elapsed) so it is testable with no
device attached -- 9 tests, including the diagonal-speed and
stalled-thread cases."
```

---

### Task 3: JoystickView — the pad

**Files:**
- Create: `android/app/src/main/java/dev/palmtop/client/JoystickView.java`
- Test: `android/app/src/test/java/dev/palmtop/client/JoystickViewTest.java`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `static float[] JoystickView.vectorFor(float touchX, float touchY, float centerX, float centerY, float radius)` → `{vx, vy}` each −1..1, magnitude clamped to 1. Pure; the unit under test.
  - `interface JoystickView.Listener { void onVector(float vx, float vy); }`
  - `JoystickView(Context c)` constructor
  - `void setListener(Listener l)`

- [ ] **Step 1: Write the failing test**

Create `android/app/src/test/java/dev/palmtop/client/JoystickViewTest.java`:

```java
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
cd /home/skapyskar/palmtop/android && JAVA_HOME=/home/skapyskar/opt/jdk-17.0.19+10 ./gradlew --offline testDebugUnitTest --tests '*JoystickViewTest*'
```
Expected: FAIL — compilation error, `cannot find symbol: class JoystickView`.

- [ ] **Step 3: Write the implementation**

Create `android/app/src/main/java/dev/palmtop/client/JoystickView.java`:

```java
package dev.palmtop.client;

import android.annotation.SuppressLint;
import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.view.MotionEvent;
import android.view.View;

/**
 * A thumb-stick for nudging the laptop's cursor.
 *
 * <p>Exists because absolute tap-to-click, which is and stays the default,
 * structurally cannot express "move two pixels". Tapping is set by where your
 * finger lands, and a fingertip is tens of pixels wide -- fine for hitting a
 * button, useless for grabbing a window edge or placing a text caret. This
 * gives that back without taking anything away: it is purely additive, and
 * tapping the video still works exactly as before.
 *
 * <p>Deliberately knows nothing about the network, the protocol or the
 * session. It reports where the stick is pushed, as a vector with each
 * component in -1..1 and the magnitude clamped to 1; {@link CursorDriver}
 * owns the question of what that should do to the cursor. The clamp/normalise
 * maths lives in {@link #vectorFor} as a static pure function so it is
 * testable on the JVM, with no device attached.
 *
 * <p>The magnitude clamp is load-bearing rather than cosmetic. Without it a
 * thumb that slides off the pad reports a magnitude of 4 or 5 and the cursor
 * bolts; and a diagonal push would read as sqrt(2) rather than 1, making
 * diagonals travel faster than cardinals at the same deflection.
 */
final class JoystickView extends View {

    /** Told where the stick is pushed. Zero vector on release. */
    interface Listener {
        void onVector(float vx, float vy);
    }

    private final Paint padPaint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint rimPaint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint nubPaint = new Paint(Paint.ANTI_ALIAS_FLAG);

    private Listener listener;

    /** Nub offset from centre, in pixels, already clamped to the pad radius. */
    private float nubOffsetX, nubOffsetY;
    private boolean held;

    JoystickView(Context context) {
        super(context);
        padPaint.setColor(Ui.RAISED);
        rimPaint.setColor(Ui.HAIRLINE);
        rimPaint.setStyle(Paint.Style.STROKE);
        rimPaint.setStrokeWidth(Ui.dp(context, 1));
        nubPaint.setColor(Ui.ACCENT);
    }

    void setListener(Listener listener) {
        this.listener = listener;
    }

    /**
     * The stick position for a touch at (touchX, touchY).
     *
     * <p>Pure -- no view state, no Android, no side effects.
     *
     * @param radius the pad's radius in pixels; 0 is tolerated (the view may
     *     not have been laid out yet) and yields the zero vector
     * @return {vx, vy}, each -1..1, magnitude clamped to 1, screen convention
     *     (down is +y)
     */
    static float[] vectorFor(float touchX, float touchY, float centerX, float centerY, float radius) {
        if (radius <= 0f) return new float[] { 0f, 0f };

        float dx = (touchX - centerX) / radius;
        float dy = (touchY - centerY) / radius;

        float magnitude = (float) Math.hypot(dx, dy);
        if (magnitude > 1f) {
            // Scale both components by the same factor: saturate the
            // magnitude while leaving the direction exactly as pushed.
            dx /= magnitude;
            dy /= magnitude;
        }
        return new float[] { dx, dy };
    }

    /** Half the shorter side, less the nub's own radius so the nub stays
     *  fully inside the pad at full deflection instead of half-escaping it. */
    private float padRadius() {
        return Math.min(getWidth(), getHeight()) / 2f - nubRadius();
    }

    private float nubRadius() {
        return Math.min(getWidth(), getHeight()) * 0.22f;
    }

    @SuppressLint("ClickableViewAccessibility") // a thumb-stick has no click semantics to announce
    @Override
    public boolean onTouchEvent(MotionEvent event) {
        float cx = getWidth() / 2f;
        float cy = getHeight() / 2f;
        float radius = padRadius();

        switch (event.getActionMasked()) {
            case MotionEvent.ACTION_DOWN:
            case MotionEvent.ACTION_MOVE: {
                held = true;
                // Claim the gesture: without this the enclosing ScrollView
                // steals any vertical drag partway through, and the stick
                // dies mid-push.
                getParent().requestDisallowInterceptTouchEvent(true);
                float[] v = vectorFor(event.getX(), event.getY(), cx, cy, radius);
                nubOffsetX = v[0] * radius;
                nubOffsetY = v[1] * radius;
                if (listener != null) listener.onVector(v[0], v[1]);
                invalidate();
                return true;
            }
            case MotionEvent.ACTION_UP:
            case MotionEvent.ACTION_CANCEL: {
                held = false;
                nubOffsetX = 0f;
                nubOffsetY = 0f;
                getParent().requestDisallowInterceptTouchEvent(false);
                // Explicitly zero rather than simply stopping: the driver
                // must be told the stick is centred, or it keeps ticking.
                if (listener != null) listener.onVector(0f, 0f);
                invalidate();
                return true;
            }
            default:
                return super.onTouchEvent(event);
        }
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        float cx = getWidth() / 2f;
        float cy = getHeight() / 2f;
        float radius = padRadius();
        float nub = nubRadius();

        canvas.drawCircle(cx, cy, radius + nub, padPaint);
        canvas.drawCircle(cx, cy, radius + nub, rimPaint);

        // Dimmed at rest so the control reads as available rather than
        // active -- the saturated accent is reserved for a stick in use.
        nubPaint.setAlpha(held ? 255 : 170);
        canvas.drawCircle(cx + nubOffsetX, cy + nubOffsetY, nub, nubPaint);
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run:
```bash
cd /home/skapyskar/palmtop/android && JAVA_HOME=/home/skapyskar/opt/jdk-17.0.19+10 ./gradlew --offline testDebugUnitTest --tests '*JoystickViewTest*'
```
Expected: `BUILD SUCCESSFUL`, 9 tests passing.

- [ ] **Step 5: Commit**

```bash
cd /home/skapyskar/palmtop
git add android/app/src/main/java/dev/palmtop/client/JoystickView.java android/app/src/test/java/dev/palmtop/client/JoystickViewTest.java
git commit -m "feat(android): add JoystickView, the cursor thumb-stick

Absolute tap-to-click cannot express 'move two pixels' -- a fingertip
is tens of pixels wide, fine for a button, useless for a window edge or
a text caret. This gives that back additively; tapping the video is
untouched.

Reports only a stick vector and knows nothing about the network;
CursorDriver owns what that does to the cursor. The magnitude clamp is
load-bearing: without it a thumb sliding off the pad reports magnitude
4 and the cursor bolts, and diagonals would read sqrt(2) and outrun
cardinals. Geometry is a static pure function, so it is tested on the
JVM with no device -- 9 tests."
```

---

### Task 4: Settings sheet and the rebuilt left column

**Files:**
- Modify: `android/app/src/main/java/dev/palmtop/client/MainActivity.java` — replace `buildLeftBar()` (lines ~299-364), add `showSettings()` / `dismissSettings()`, update `setControlsVisible()` (lines ~537-545), add a `settingsOverlayView` field.

**Interfaces:**
- Consumes: `Ui.*` factories; existing `startConnection()`, `openDeviceList()`, `showModePicker()`, `showAspectPicker()`, `showSessionLog()`, `updateModeButton()`, `updateAspectButton()`, `showKeyboard()`, `hud`.
- Produces: `buildLeftBar()` now creates a `joystickSlot` `LinearLayout` placeholder that Task 5 fills; fields `settingsButton`, `kbToggle` remain; `statusView`, `reconnectButton`, `modeButton`, `aspectButton`, `logButton`, `hudToggle` still exist as fields but are built inside the settings sheet.

**Critical:** `statusView`, `modeButton` and `aspectButton` are written to from other methods (`startConnection`, `handleStatus`, `runNetwork`'s catch, `updateModeButton`, `updateAspectButton`). Those methods must keep working when the sheet is closed and the views are detached. Guard every such write with a null check, and re-apply current text on sheet open.

- [ ] **Step 1: Add the fields**

Near the existing `private View logOverlayView;` declaration (~line 115), add:

```java
    private View settingsOverlayView;
    private Button settingsButton;
    /** Empty container in the left column that Task 5's joystick fills.
     *  Kept as a field so the column's construction stays one readable
     *  method rather than growing a second responsibility. */
    private LinearLayout joystickSlot;
    /** Last status line and its colour. The status view lives in the
     *  settings sheet now, which is detached most of the time -- so the
     *  text has to survive independently of the view that shows it, or
     *  opening Settings would show a blank line until something next
     *  happened to update it. */
    private String statusText = "";
    private int statusColor = Ui.TEXT_MUTED;
```

- [ ] **Step 2: Replace `buildLeftBar()`**

Replace the whole existing `buildLeftBar()` method (from its javadoc `/** Every control lives in this one column: ... */` through its closing brace, and the `iconSlot` helper immediately after it) with:

```java
    /**
     * The always-visible controls: settings, keyboard, and the cursor
     * joystick.
     *
     * <p>Everything that is set once and then forgotten -- connection status,
     * reconnect, devices, quality mode, aspect ratio, the log and the stats
     * toggle -- moved behind {@link #showSettings()}. They were permanently
     * occupying the column that the controls used *during* a session should
     * own, which is what made room for the joystick without touching the
     * video.
     *
     * <p>The column keeps its existing fixed width, so the video's width and
     * shape are exactly what they were before this change. That was the
     * hard constraint on this whole redesign.
     */
    private LinearLayout buildLeftBar() {
        LinearLayout bar = new LinearLayout(this);
        bar.setOrientation(LinearLayout.VERTICAL);
        bar.setBackgroundColor(Ui.PANEL);
        bar.setPadding(Ui.md(this), Ui.md(this), Ui.md(this), Ui.md(this));

        LinearLayout topRow = new LinearLayout(this);
        topRow.setOrientation(LinearLayout.HORIZONTAL);

        settingsButton = Ui.iconButton(this, "⚙");
        settingsButton.setOnClickListener(v -> showSettings());
        topRow.addView(settingsButton, iconSlot(0));

        kbToggle = Ui.iconButton(this, "⌨");
        kbToggle.setOnClickListener(v -> showKeyboard());
        topRow.addView(kbToggle, iconSlot(Ui.dp(this, 6)));

        bar.addView(topRow, Ui.stacked(this, 10));

        // Filled by wireJoystick(). Empty here so this method stays "build
        // the column" rather than also "construct and wire an input device".
        joystickSlot = new LinearLayout(this);
        joystickSlot.setOrientation(LinearLayout.VERTICAL);
        bar.addView(joystickSlot, Ui.stacked(this, 10));

        // Below the controls, not above them: the HUD is diagnostic, appears
        // only when toggled on, and would otherwise shove every button down
        // the column the moment it did. It stays in the column rather than
        // moving into the settings sheet because it is meant to be read
        // *while* using the session -- only its toggle moved.
        hud = new HudView(this);
        bar.addView(hud, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));

        return bar;
    }

    /** Equal-width slot in an icon row. Weighted with a zero base width so
     *  the buttons share the column exactly, whatever glyph each carries. */
    private LinearLayout.LayoutParams iconSlot(int leftMargin) {
        LinearLayout.LayoutParams lp =
                new LinearLayout.LayoutParams(0, Ui.dp(this, 42), 1f);
        lp.leftMargin = leftMargin;
        return lp;
    }
```

- [ ] **Step 3: Add `showSettings()` and `dismissSettings()`**

Add these methods immediately after `dismissSessionLog()`:

```java
    /**
     * Everything that is configured rather than operated.
     *
     * <p>Built fresh on each open rather than kept around: these controls are
     * touched rarely, so holding an inflated sheet for the whole session to
     * save a few milliseconds on an occasional tap is the wrong trade. It
     * also means the sheet always reflects current state by construction,
     * with no separate refresh path to keep in sync.
     */
    private void showSettings() {
        if (settingsOverlayView != null) {
            dismissSettings();
            return;
        }
        LinearLayout overlay = Ui.sheet(this);

        overlay.addView(Ui.title(this, "Settings"), Ui.stacked(this, 12));
        overlay.addView(Ui.hairline(this), new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, Ui.dp(this, 1)));

        LinearLayout content = new LinearLayout(this);
        content.setOrientation(LinearLayout.VERTICAL);
        ScrollView scroll = new ScrollView(this);
        scroll.setPadding(0, Ui.md(this), 0, Ui.md(this));
        scroll.setClipToPadding(false);
        scroll.addView(content);
        overlay.addView(scroll, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f));

        // The connection line. Recreated here and immediately re-populated
        // from the retained statusText/statusColor -- the view is detached
        // whenever this sheet is closed, so the *text* has to outlive it.
        statusView = Ui.mono(this);
        statusView.setTextColor(statusColor);
        statusView.setText(statusText);
        content.addView(statusView, Ui.stacked(this, 12));

        reconnectButton = Ui.button(this, "⟳  Reconnect");
        reconnectButton.setOnClickListener(v -> {
            dismissSettings();
            startConnection();
        });
        content.addView(reconnectButton, Ui.stacked(this, 6));

        devicesButton = Ui.button(this, "🖥  Devices");
        devicesButton.setOnClickListener(v -> {
            dismissSettings();
            openDeviceList();
        });
        content.addView(devicesButton, Ui.stacked(this, 6));

        modeButton = Ui.button(this, "");
        modeButton.setOnClickListener(v -> showModePicker());
        content.addView(modeButton, Ui.stacked(this, 6));
        updateModeButton();

        aspectButton = Ui.button(this, "");
        aspectButton.setOnClickListener(v -> showAspectPicker());
        content.addView(aspectButton, Ui.stacked(this, 6));
        updateAspectButton();

        logButton = Ui.button(this, "📋  Session log");
        logButton.setOnClickListener(v -> {
            dismissSettings();
            showSessionLog();
        });
        content.addView(logButton, Ui.stacked(this, 6));

        hudToggle = Ui.button(this, hud.isHudShown() ? "📊  Hide stats" : "📊  Show stats");
        hudToggle.setOnClickListener(v -> {
            hud.setShown(!hud.isHudShown());
            hudToggle.setText(hud.isHudShown() ? "📊  Hide stats" : "📊  Show stats");
        });
        content.addView(hudToggle, Ui.stacked(this, 0));

        Button close = Ui.primaryButton(this, "Close");
        close.setOnClickListener(v -> dismissSettings());
        overlay.addView(close);

        settingsOverlayView = overlay;
        rootLayout.addView(overlay, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
    }

    private void dismissSettings() {
        if (settingsOverlayView != null) {
            rootLayout.removeView(settingsOverlayView);
            settingsOverlayView = null;
        }
    }

    /**
     * Records the connection line and shows it if the settings sheet happens
     * to be open.
     *
     * <p>Single funnel for every status update, because {@link #statusView}
     * now lives in a sheet that is detached most of the time. Writing to the
     * view directly would either NPE or silently update a view nobody can
     * see, and the next open would show a stale or blank line.
     */
    private void setStatus(int color, String text) {
        statusColor = color;
        statusText = text;
        if (statusView != null && settingsOverlayView != null) {
            statusView.setTextColor(color);
            statusView.setText(text);
        }
    }
```

- [ ] **Step 4: Route every status write through `setStatus`**

There are five call sites. Replace each pair of `statusView.setTextColor(...)` / `statusView.setText(...)` lines:

In `startConnection()`:
```java
        setStatus(Ui.ACCENT, "connecting to " + host + ":" + port + " ...");
```

In `runNetwork()`'s catch block (was `statusView.setTextColor(Ui.ERR)` / `setText("ERROR: " + e + ...)`):
```java
                    setStatus(Ui.ERR, "ERROR: " + e + "\n\ntap ⚙ → Reconnect to retry");
```

In `handleStatus()` (was the `failed ? ... : ...` pair):
```java
            setStatus(failed ? Ui.ERR : Ui.TEXT_MUTED,
                    failed ? ("ERROR: " + detail) : detail);
```

In `handleVideoConfig()` (old lines 1527-1529), replace:
```java
                statusView.setTextColor(Ui.OK);
                statusView.setText("● connected  " + host + ":" + port + "\n"
                        + cfg.width + "x" + cfg.height + "@" + cfg.fps + "fps");
```
with:
```java
                setStatus(Ui.OK, "● connected  " + host + ":" + port + "\n"
                        + cfg.width + "x" + cfg.height + "@" + cfg.fps + "fps");
```

In the periodic stats reporter (old lines 1605-1608), replace:
```java
            statusView.setTextColor(Ui.TEXT_MUTED);
            statusView.setText(host + ":" + port + "\n"
                    + vc.width + "x" + vc.height + "@" + vc.fps + "fps\n"
                    + "decoded " + d + "   stale " + sk);
```
with:
```java
            setStatus(Ui.TEXT_MUTED, host + ":" + port + "\n"
                    + vc.width + "x" + vc.height + "@" + vc.fps + "fps\n"
                    + "decoded " + d + "   stale " + sk);
```

Verify none remain:
```bash
grep -n "statusView\." /home/skapyskar/palmtop/android/app/src/main/java/dev/palmtop/client/MainActivity.java
```
Expected: only the three lines inside `showSettings()` and `setStatus()`.

- [ ] **Step 5: Update `setControlsVisible()`**

Replace its body with:

```java
    private void setControlsVisible(boolean visible) {
        int v = visible ? View.VISIBLE : View.GONE;
        // Only the always-visible column controls need toggling now. The rest
        // live in the settings sheet, which is a separate overlay that is
        // simply not open while the device list covers the screen.
        settingsButton.setVisibility(v);
        kbToggle.setVisibility(v);
        joystickSlot.setVisibility(v);
    }
```

- [ ] **Step 6: Guard `updateModeButton` / `updateAspectButton`**

They already null-check (`if (modeButton != null)`). Confirm both do; if `updateAspectButton` does not, add the same guard. These are now called while the buttons may be detached.

- [ ] **Step 7: Build and run the full test suite**

Run:
```bash
cd /home/skapyskar/palmtop/android && JAVA_HOME=/home/skapyskar/opt/jdk-17.0.19+10 ./gradlew --offline testDebugUnitTest
```
Expected: `BUILD SUCCESSFUL`, all tests passing.

- [ ] **Step 8: Commit**

```bash
cd /home/skapyskar/palmtop
git add android/app/src/main/java/dev/palmtop/client/MainActivity.java
git commit -m "feat(android): move set-once controls behind a settings sheet

Status, reconnect, devices, quality mode, aspect, log and the stats
toggle were permanently occupying the left column despite being
set-once-and-forget. They move into a sheet behind a gear; the column
keeps only what is used during a session.

The column's width is deliberately unchanged, so the video's width and
aspect ratio are exactly what they were.

Status text is now retained separately from the view that shows it and
funnelled through setStatus(): the status line lives in a sheet that is
detached most of the time, so writing to the view directly would either
NPE or update something nobody can see. The HUD itself stays in the
column -- it is meant to be read while using the session, so only its
toggle moved."
```

---

### Task 5: Wire the joystick into the column

**Files:**
- Modify: `android/app/src/main/java/dev/palmtop/client/MainActivity.java` — add `wireJoystick()`, call it from `buildUi()`, add button-release-on-teardown to `teardown()`.

**Interfaces:**
- Consumes: `JoystickView(Context)`, `JoystickView.setListener(Listener)`, `CursorDriver(Sink)`, `CursorDriver.setVector(float,float)`, `CursorDriver.stop()`, `Protocol.pointerMotionRelative(float,float)`, `Protocol.pointerButton(int,boolean)`, `Protocol.BUTTON_LEFT`, `Protocol.BUTTON_RIGHT`, the `joystickSlot` field from Task 4.
- Produces: final feature.

- [ ] **Step 1: Add the fields**

Next to the `pinchZoom` field:

```java
    private CursorDriver cursorDriver;
    /** Pointer buttons this client currently believes are pressed on the
     *  host, indexed by Protocol.BUTTON_*. Tracked so a connection that
     *  drops mid-drag cannot leave the laptop with a stuck mouse button --
     *  the same class of bug as the plan's 'modifier stuck' edge case. */
    private final boolean[] buttonHeld = new boolean[3];
```

- [ ] **Step 2: Add `wireJoystick()`**

Add after `buildLeftBar()`:

```java
    /**
     * Builds the joystick and its click buttons into the slot the column
     * left for them.
     *
     * <p>Separate from {@link #buildLeftBar()} because it is doing something
     * different in kind: that method arranges views, this one assembles an
     * input device out of three parts and connects it to the wire.
     *
     * <p>Nothing here needed a protocol change. {@code PointerMotionRelative}
     * and {@code PointerButton} have both been implemented on the host since
     * before this feature existed, so the joystick cannot desync the two ends
     * and this ships as an APK-only change.
     */
    private void wireJoystick() {
        cursorDriver = new CursorDriver((dx, dy) ->
                enqueue(Protocol.pointerMotionRelative(dx, dy)));

        JoystickView stick = new JoystickView(this);
        stick.setListener(cursorDriver::setVector);
        int size = Ui.dp(this, 128);
        LinearLayout.LayoutParams stickLp = new LinearLayout.LayoutParams(size, size);
        stickLp.gravity = Gravity.CENTER_HORIZONTAL;
        stickLp.bottomMargin = Ui.sm(this);
        joystickSlot.addView(stick, stickLp);

        LinearLayout clicks = new LinearLayout(this);
        clicks.setOrientation(LinearLayout.HORIZONTAL);

        Button left = Ui.iconButton(this, "L");
        wireClickButton(left, Protocol.BUTTON_LEFT);
        clicks.addView(left, iconSlot(0));

        Button right = Ui.iconButton(this, "R");
        wireClickButton(right, Protocol.BUTTON_RIGHT);
        clicks.addView(right, iconSlot(Ui.dp(this, 6)));

        joystickSlot.addView(clicks, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));
    }

    /**
     * Sends press and release as the finger goes down and up, rather than
     * synthesising a click on tap.
     *
     * <p>That distinction is the whole reason dragging works: holding L while
     * working the joystick produces a real press-move-release on the host, so
     * you can resize a window edge or select text -- something absolute
     * tap-to-click cannot express at all.
     */
    @SuppressLint("ClickableViewAccessibility") // press/release is the point; a click callback cannot express it
    private void wireClickButton(Button button, int protoButton) {
        button.setOnTouchListener((v, event) -> {
            switch (event.getActionMasked()) {
                case MotionEvent.ACTION_DOWN:
                    v.setPressed(true);
                    sendButton(protoButton, true);
                    return true;
                case MotionEvent.ACTION_UP:
                case MotionEvent.ACTION_CANCEL:
                    v.setPressed(false);
                    sendButton(protoButton, false);
                    return true;
                default:
                    return false;
            }
        });
    }

    /** Sends a pointer button and remembers it, so {@link #teardown()} can
     *  release anything still held if the session ends mid-press. */
    private void sendButton(int protoButton, boolean pressed) {
        if (protoButton >= 0 && protoButton < buttonHeld.length) {
            buttonHeld[protoButton] = pressed;
        }
        enqueue(Protocol.pointerButton(protoButton, pressed));
    }
```

- [ ] **Step 3: Call it from `buildUi()`**

In `buildUi()`, immediately after the `mainLayout.addView(buildVideoContainer(), ...)` line and before `buildHiddenInput(root);`, add:

```java
        // After buildLeftBar(), which creates the slot this fills.
        wireJoystick();
```

- [ ] **Step 4: Release held buttons in `teardown()`**

At the very top of `teardown()`, before `connected = false;`:

```java
        // Release anything still held before the socket goes. A connection
        // dropped mid-drag would otherwise leave the laptop with a stuck
        // mouse button and no way to clear it from this end.
        for (int b = 0; b < buttonHeld.length; b++) {
            if (buttonHeld[b]) {
                buttonHeld[b] = false;
                if (connected) enqueue(Protocol.pointerButton(b, false));
            }
        }
        if (cursorDriver != null) cursorDriver.stop();
```

- [ ] **Step 5: Add the missing imports**

Ensure `MainActivity.java` imports these (add any that are absent):

```java
import android.annotation.SuppressLint;
```
`Gravity`, `MotionEvent`, `Button`, `LinearLayout`, `ScrollView`, `FrameLayout` and `View` are already imported.

- [ ] **Step 6: Build and run the full test suite**

Run:
```bash
cd /home/skapyskar/palmtop/android && JAVA_HOME=/home/skapyskar/opt/jdk-17.0.19+10 ./gradlew --offline testDebugUnitTest assembleDebug
```
Expected: `BUILD SUCCESSFUL`, all tests passing, APK produced.

- [ ] **Step 7: Commit**

```bash
cd /home/skapyskar/palmtop
git add android/app/src/main/java/dev/palmtop/client/MainActivity.java
git commit -m "feat(android): wire the cursor joystick and L/R buttons

Joystick motion goes out as PointerMotionRelative, the buttons as
PointerButton -- both already implemented on the host since before this
feature, so no protocol bump and no version-skew risk.

The buttons send press on finger-down and release on finger-up rather
than synthesising a click, which is what makes dragging work: hold L
while working the stick and you get a real press-move-release on the
laptop, so window edges and text selection become possible. Absolute
tap-to-click is untouched.

Held buttons are tracked and released in teardown(), so a connection
dropping mid-drag cannot leave the laptop with a stuck mouse button."
```

---

### Task 6: Update the README

**Files:**
- Modify: `README.md` — the "Using it" section (lines ~121-135).

- [ ] **Step 1: Rewrite the controls section**

Replace the paragraph beginning "Tap where you want to click" and the table that follows it, through the "Three fingers" paragraph, with:

```markdown
Tap where you want to click — it works like a touchscreen, not like a laptop
trackpad. Drag to drag. The **⌨** button opens the keyboard.

The left column keeps only what you use during a session:

| | |
|---|---|
| **⚙** | Settings — everything below |
| **⌨** | Keyboard |
| **Joystick** | Nudge the cursor precisely, for window edges and text carets |
| **L** / **R** | Left and right click, wherever the cursor currently is |

Tapping the video still clicks exactly where you tapped — the joystick is an
addition, not a replacement. Because the L button sends a real press and
release, **holding L while moving the joystick drags**, which is how you resize
a window or select text.

Everything else lives behind **⚙**:

| | |
|---|---|
| **Status** | Whether you are connected, and to what |
| **⟳ Reconnect** | Retry after a network hiccup |
| **🖥 Devices** | Switch laptops, or pair another |
| **⚙ Mode** | Quality preset — see below |
| **▭ Aspect** | Best Fit / 16:9 / 4:3 / 1:1 |
| **📋 Session log** | What the laptop is doing, and what failed |
| **📊 Stats** | Live latency figures |

**Three fingers** on the video zooms and pans, for reading something small.
One-finger taps keep working normally.
```

- [ ] **Step 2: Fix the stale reference in the troubleshooting section**

The "screen stays black" section mentions "the app has a **📋 log button**". Update it to say the log is under **⚙ → Session log**.

- [ ] **Step 3: Commit**

```bash
cd /home/skapyskar/palmtop
git add README.md
git commit -m "docs: describe the settings sheet and cursor joystick"
```

---

## Verification before claiming done

- [ ] `./gradlew --offline testDebugUnitTest` passes, with the 18 new tests among them
- [ ] `./gradlew --offline assembleDebug` produces an APK
- [ ] `grep -n "statusView\." MainActivity.java` shows writes only inside `showSettings()` and `setStatus()`
- [ ] `git diff <base> -- crates/ android/app/src/main/java/dev/palmtop/client/Protocol.java` is **empty** — proving no protocol or host change crept in
- [ ] The control column width literal `170` is unchanged in `buildUi()`

**Cannot be verified in this environment, and must be reported as such rather than implied:** there is no emulator or attached device here. Whether the joystick *feels* right — the deadzone, the curve, the 1200 px/s ceiling — and whether 12dp is the right top inset are open questions answerable only by using it. The tuning constants are named and colocated in `CursorDriver` specifically so that is a one-line change.
