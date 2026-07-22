package dev.palmtop.spike;

/**
 * Pure scale/pan bookkeeping for the 3-finger zoom gesture -- deliberately
 * free of Android imports (no MotionEvent, no View) so the arithmetic runs
 * under a plain JVM test, same reasoning as {@link VideoFit} and
 * {@link LatencyTracker}. {@link PinchZoomController} is the thin Android
 * wrapper that extracts pointer coordinates from a MotionEvent and calls in
 * here.
 *
 * <h3>Why gestures are "segments" that fold into a persisted base</h3>
 * A real pinch rarely holds a fixed 3 fingers for its whole duration -- a 4th
 * joins, one lifts and rejoins, etc. Recomputing the scale/pan delta against
 * the *original* reference distance/centroid across such a change would jump
 * discontinuously, because the reference itself no longer describes the
 * current pointer set. Instead, every time the pointer set changes,
 * {@link #endSegment()} folds whatever the gesture has produced so far into
 * a persisted base, and {@link #beginSegment} starts a fresh reference from
 * the new pointer set -- the visible scale/pan never jumps, only the
 * internal bookkeeping resets. The same mechanism is what lets one pinch
 * continue smoothly from where the previous one left off, across the whole
 * app session.
 */
final class PinchZoomMath {
    /** Can't zoom below the base fit -- there is nothing gained by shrinking
     *  the already-optimal placement further. */
    static final float MIN_SCALE = 1.0f;
    /** Generous but bounded -- past this the picture is mostly blur, not
     *  useful precision, on any phone screen. */
    static final float MAX_SCALE = 6.0f;

    private float baseScale = 1.0f;
    private float basePanX = 0f;
    private float basePanY = 0f;

    private float refDistance;
    private float refCentroidX;
    private float refCentroidY;

    private float currentScale = 1.0f;
    private float currentPanX = 0f;
    private float currentPanY = 0f;

    /** Starts (or re-baselines) a gesture segment from the current pointer
     * set's spread and centroid. */
    void beginSegment(float distance, float centroidX, float centroidY) {
        refDistance = distance;
        refCentroidX = centroidX;
        refCentroidY = centroidY;
        currentScale = baseScale;
        currentPanX = basePanX;
        currentPanY = basePanY;
    }

    /**
     * Recomputes the current (not-yet-persisted) scale/pan from a new
     * pointer spread/centroid, clamped so the result never zooms out past
     * the base fit, past {@link #MAX_SCALE}, or pans far enough to reveal
     * empty space beyond the video content.
     *
     * @param contentWidth  the video surface's own (unscaled) layout width --
     *                      what {@code scale} multiplies
     * @param contentHeight the video surface's own (unscaled) layout height
     * @param visibleWidth  the on-screen rect that must stay fully covered
     * @param visibleHeight ditto, height
     */
    void update(float distance, float centroidX, float centroidY,
                int contentWidth, int contentHeight, int visibleWidth, int visibleHeight) {
        float scaleDelta = refDistance > 0 ? distance / refDistance : 1.0f;
        currentScale = clamp(baseScale * scaleDelta, MIN_SCALE, MAX_SCALE);

        float panDeltaX = centroidX - refCentroidX;
        float panDeltaY = centroidY - refCentroidY;

        // How far the scaled content can be shifted before its edge would
        // retreat inside the visible rect's edge, leaving a gap. At
        // scale==1 with no baked-in crop margin (contentWidth==visibleWidth)
        // this is exactly 0 -- panning is correctly disallowed when there is
        // nothing extra to reveal.
        float maxPanX = Math.max(0f, (currentScale * contentWidth - visibleWidth) / 2f);
        float maxPanY = Math.max(0f, (currentScale * contentHeight - visibleHeight) / 2f);

        currentPanX = clamp(basePanX + panDeltaX, -maxPanX, maxPanX);
        currentPanY = clamp(basePanY + panDeltaY, -maxPanY, maxPanY);
    }

    /** Folds the current (in-progress) scale/pan into the persisted base, so
     * the next {@link #beginSegment} continues from here rather than
     * jumping back to whatever the base was before this segment. */
    void endSegment() {
        baseScale = currentScale;
        basePanX = currentPanX;
        basePanY = currentPanY;
    }

    /** Hard reset to the identity (no zoom, no pan) -- called whenever the
     * base fit itself changes (aspect mode or stream resolution), since the
     * coordinate system a prior zoom/pan was measured against no longer
     * exists. */
    void reset() {
        baseScale = currentScale = 1.0f;
        basePanX = currentPanX = 0f;
        basePanY = currentPanY = 0f;
    }

    float getScale() { return currentScale; }
    float getPanX() { return currentPanX; }
    float getPanY() { return currentPanY; }

    private static float clamp(float v, float lo, float hi) {
        return Math.max(lo, Math.min(hi, v));
    }
}
