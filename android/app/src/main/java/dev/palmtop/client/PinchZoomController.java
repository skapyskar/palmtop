package dev.palmtop.client;

import android.view.MotionEvent;
import android.view.View;

/**
 * Detects a 3-or-more-finger gesture on the video and turns it into a local
 * zoom/pan applied to {@code target} -- purely a phone-side magnification of
 * the already-decoded picture, never sent to the host. All the actual
 * arithmetic lives in {@link PinchZoomMath}; this class only extracts pointer
 * coordinates from Android's {@link MotionEvent} and drives that state
 * machine.
 *
 * <h3>Once a gesture goes 3+ fingers, it stays a zoom gesture</h3>
 * If it dropped back to normal click handling the moment a finger lifted
 * (even mid-pinch, one at a time), lifting fingers off a pinch one by one
 * would fire a stray click at whatever position the last remaining finger
 * happened to be. Once {@link #onTouch} sees 3+ pointers, it keeps consuming
 * every event for that entire touch sequence -- caller sees "not a click" --
 * until every finger is off the glass (a real {@code ACTION_UP}, not just an
 * {@code ACTION_POINTER_UP} for one of several).
 */
final class PinchZoomController {
    private final View target;
    private final PinchZoomMath math = new PinchZoomMath();

    private boolean zooming = false;
    private int contentWidth, contentHeight, visibleWidth, visibleHeight;

    PinchZoomController(View target) {
        this.target = target;
    }

    /** Must be called every time the base placement changes (aspect mode or
     * stream resolution) -- see {@link PinchZoomMath#reset} for why. */
    void reset() {
        math.reset();
        applyToTarget();
    }

    /** The video surface's own (unscaled) layout size and the on-screen
     * visible rect it's centered within -- see {@link PinchZoomMath#update}
     * for what each is used for. */
    void setContentSize(int contentWidth, int contentHeight, int visibleWidth, int visibleHeight) {
        this.contentWidth = contentWidth;
        this.contentHeight = contentHeight;
        this.visibleWidth = visibleWidth;
        this.visibleHeight = visibleHeight;
    }

    /**
     * @return true if this event was consumed as part of a zoom/pan gesture
     *     (caller must not treat it as a click/drag); false if it's an
     *     ordinary sub-3-finger touch the caller should handle itself.
     */
    boolean onTouch(MotionEvent event) {
        int action = event.getActionMasked();

        if (action == MotionEvent.ACTION_DOWN) {
            zooming = false; // a fresh sequence always starts as a potential tap
        }

        if (!zooming && event.getPointerCount() >= 3) {
            zooming = true;
            float[] c = centroidAndSpread(event);
            math.beginSegment(c[2], c[0], c[1]);
        }

        if (!zooming) return false;

        if (action == MotionEvent.ACTION_POINTER_DOWN || action == MotionEvent.ACTION_POINTER_UP) {
            // The pointer set is about to change (or just did) -- fold
            // what's accumulated so far and re-baseline from the new set,
            // rather than let the reference distance/centroid (which
            // described the *old* set) produce a discontinuous jump.
            math.endSegment();
            float[] c = centroidAndSpread(event);
            math.beginSegment(c[2], c[0], c[1]);
        } else if (action == MotionEvent.ACTION_MOVE) {
            float[] c = centroidAndSpread(event);
            math.update(c[2], c[0], c[1], contentWidth, contentHeight, visibleWidth, visibleHeight);
            applyToTarget();
        } else if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
            math.endSegment();
            zooming = false;
        }
        return true;
    }

    private void applyToTarget() {
        float scale = math.getScale();
        target.setScaleX(scale);
        target.setScaleY(scale);
        target.setTranslationX(math.getPanX());
        target.setTranslationY(math.getPanY());
    }

    /**
     * @return {centroidX, centroidY, spread}, where spread is the average
     *     distance from each pointer to the centroid -- an O(n) stand-in for
     *     average pairwise distance that works as well for a scale signal
     *     with 3+ fingers and avoids the O(n^2) pairwise computation.
     */
    private static float[] centroidAndSpread(MotionEvent event) {
        int n = event.getPointerCount();
        float cx = 0, cy = 0;
        for (int i = 0; i < n; i++) {
            cx += event.getX(i);
            cy += event.getY(i);
        }
        cx /= n;
        cy /= n;

        float spread = 0;
        for (int i = 0; i < n; i++) {
            float dx = event.getX(i) - cx;
            float dy = event.getY(i) - cy;
            spread += (float) Math.sqrt(dx * dx + dy * dy);
        }
        spread /= n;

        return new float[] { cx, cy, spread };
    }
}
