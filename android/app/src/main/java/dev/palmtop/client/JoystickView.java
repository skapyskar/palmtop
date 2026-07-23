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
