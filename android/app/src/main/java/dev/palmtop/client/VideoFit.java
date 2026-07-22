package dev.palmtop.client;

/**
 * Computes the largest rectangle that fits inside an available area while
 * preserving a content's aspect ratio -- the "contain"/letterbox fit used to
 * size the video surface against the laptop's actual resolution.
 *
 * Deliberately free of Android imports (no {@code android.util.Size}) so it
 * runs under a plain JVM unit test, the same reasoning as
 * {@link LatencyTracker}: this is exactly the kind of integer/ratio
 * arithmetic that can be subtly wrong (an off-by-one rounding direction, an
 * inverted comparison) while still producing a plausible-looking rectangle on
 * a running device. A wrong fit either clips part of the desktop or leaves an
 * unnecessary gap -- both look like "the app is a bit off" rather than
 * failing loudly, so it needs to be verified against known cases, not eyeballed.
 */
final class VideoFit {

    /** Plain result holder -- not android.util.Size, for the same
     *  JVM-testability reason as the rest of this class. */
    static final class Size {
        final int width;
        final int height;

        Size(int width, int height) {
            this.width = width;
            this.height = height;
        }

        @Override
        public boolean equals(Object o) {
            if (!(o instanceof Size)) return false;
            Size other = (Size) o;
            return width == other.width && height == other.height;
        }

        @Override
        public int hashCode() {
            return 31 * width + height;
        }

        @Override
        public String toString() {
            return width + "x" + height;
        }
    }

    /**
     * @param containerWidth  available width, px
     * @param containerHeight available height, px
     * @param contentWidth    the content's native width (e.g. the laptop's
     *                        stream resolution)
     * @param contentHeight   the content's native height
     * @return the largest {@code contentWidth:contentHeight}-ratio rectangle
     *     that fits within the container, centered. Never stretches the
     *     content past its own aspect ratio -- only scales it uniformly.
     *     Falls back to filling the container if any dimension is
     *     non-positive (nothing sane to compute yet, e.g. before the first
     *     layout pass or before a VideoConfig has arrived).
     */
    static Size fit(int containerWidth, int containerHeight, int contentWidth, int contentHeight) {
        if (containerWidth <= 0 || containerHeight <= 0 || contentWidth <= 0 || contentHeight <= 0) {
            return new Size(Math.max(containerWidth, 0), Math.max(containerHeight, 0));
        }

        // Compare the two aspect ratios without floating-point division by
        // cross-multiplying: containerW/containerH vs contentW/contentH
        // becomes containerW*contentH vs contentW*containerH. Avoids any
        // rounding surprise at the comparison itself; only the final
        // dimension needs rounding, and only once.
        long containerRatio = (long) containerWidth * contentHeight;
        long contentRatio = (long) contentWidth * containerHeight;

        if (containerRatio > contentRatio) {
            // Container is proportionally wider than the content -- height is
            // the binding dimension (pillarbox: bars appear left/right).
            int width = (int) Math.round((double) contentWidth * containerHeight / contentHeight);
            return new Size(width, containerHeight);
        } else if (containerRatio < contentRatio) {
            // Container is proportionally taller -- width is binding
            // (letterbox: bars appear top/bottom).
            int height = (int) Math.round((double) contentHeight * containerWidth / contentWidth);
            return new Size(containerWidth, height);
        } else {
            // Ratios match exactly -- fill the container with no bars at all.
            return new Size(containerWidth, containerHeight);
        }
    }

    /**
     * Everything needed to place the video surface when it may be cropped to
     * a different shape than its native one (see {@link AspectMode}).
     */
    static final class Placement {
        /** LayoutParams size for the SurfaceView itself. Equal to
         *  {@code visibleWidth}/{@code visibleHeight} when nothing is
         *  cropped (Best Fit); larger on whichever axis is being cropped
         *  otherwise -- the surface is deliberately rendered oversized and
         *  centered so the parent's ordinary child-clipping trims exactly
         *  the cropped-away margins, with no matrix/canvas work needed. */
        final int surfaceWidth;
        final int surfaceHeight;
        /** The actual on-screen visible rectangle -- always
         *  {@code <= container} and always in the *target* ratio. This is
         *  the rect a pinch-zoom needs to know the size of, since it's what
         *  "fully zoomed out" should exactly cover. */
        final int visibleWidth;
        final int visibleHeight;

        Placement(int surfaceWidth, int surfaceHeight, int visibleWidth, int visibleHeight) {
            this.surfaceWidth = surfaceWidth;
            this.surfaceHeight = surfaceHeight;
            this.visibleWidth = visibleWidth;
            this.visibleHeight = visibleHeight;
        }

        @Override
        public boolean equals(Object o) {
            if (!(o instanceof Placement)) return false;
            Placement p = (Placement) o;
            return surfaceWidth == p.surfaceWidth && surfaceHeight == p.surfaceHeight
                    && visibleWidth == p.visibleWidth && visibleHeight == p.visibleHeight;
        }

        @Override
        public int hashCode() {
            return ((surfaceWidth * 31 + surfaceHeight) * 31 + visibleWidth) * 31 + visibleHeight;
        }

        @Override
        public String toString() {
            return "surface=" + surfaceWidth + "x" + surfaceHeight
                    + " visible=" + visibleWidth + "x" + visibleHeight;
        }
    }

    /**
     * Places the video when the user has chosen a target ratio that may
     * differ from the stream's own ({@link AspectMode}'s non-Best-Fit
     * presets). Generalises {@link #fit}: passing the video's own native
     * ratio as the target (what Best Fit does) makes this degrade to a
     * plain contain-fit with no cropping at all, so callers never need to
     * special-case Best Fit.
     *
     * <h3>How the crop is expressed as a plain LayoutParams size</h3>
     * A `SurfaceView` always renders its *entire* decoded buffer stretched to
     * fill whatever size its LayoutParams give it -- there is no way to ask
     * it to show only part of a frame. So "crop to 4:3" is achieved
     * indirectly: size the surface *larger* than the visible rect on
     * whichever axis is being cropped, by exactly the inverse of the crop
     * fraction, then center it. The parent ViewGroup's default child-clipping
     * trims the overflow, and the effect is identical to a real crop --
     * verified by construction below, not just visually.
     *
     * @param targetRatioWidth  numerator of the desired displayed shape
     * @param targetRatioHeight denominator of the desired displayed shape
     */
    static Placement computePlacement(int containerWidth, int containerHeight,
                                       int videoWidth, int videoHeight,
                                       int targetRatioWidth, int targetRatioHeight) {
        if (containerWidth <= 0 || containerHeight <= 0 || videoWidth <= 0 || videoHeight <= 0
                || targetRatioWidth <= 0 || targetRatioHeight <= 0) {
            int w = Math.max(containerWidth, 0), h = Math.max(containerHeight, 0);
            return new Placement(w, h, w, h);
        }

        // targetRatio vs nativeRatio, cross-multiplied to avoid floating
        // point in the comparison -- same reasoning as fit()'s own compare.
        // targetRatio > nativeRatio  <=>  Rw/Rh > Wv/Hv  <=>  Rw*Hv > Rh*Wv
        long nativeCross = (long) videoWidth * targetRatioHeight;   // Wv*Rh
        long targetCross = (long) targetRatioWidth * videoHeight;   // Rw*Hv

        double croppedWidth, croppedHeight;
        if (targetCross > nativeCross) {
            // Target is proportionally wider than the source -- crop off
            // the top/bottom margins, keep the full width.
            croppedWidth = videoWidth;
            croppedHeight = (double) videoWidth * targetRatioHeight / targetRatioWidth;
        } else if (targetCross < nativeCross) {
            // Target is proportionally taller -- crop left/right, keep the
            // full height.
            croppedHeight = videoHeight;
            croppedWidth = (double) videoHeight * targetRatioWidth / targetRatioHeight;
        } else {
            // Equal ratios -- including Best Fit, always -- no crop at all.
            croppedWidth = videoWidth;
            croppedHeight = videoHeight;
        }

        // The cropped (target-ratio-shaped) content, contain-fit into the
        // real container -- this is the actual visible rectangle.
        Size visible = fit(containerWidth, containerHeight,
                (int) Math.round(croppedWidth), (int) Math.round(croppedHeight));

        // Scale the FULL native frame by the same factor that took the
        // cropped slice to `visible`'s size, so that slice -- once the
        // oversized surface is centered and clipped -- lands exactly on
        // `visible`. On an axis with no cropping (croppedX == videoX
        // exactly) this multiplier is 1 and surfaceX == visibleX, i.e. no
        // oversizing happens where none is needed.
        int surfaceWidth = (int) Math.round(videoWidth * (visible.width / croppedWidth));
        int surfaceHeight = (int) Math.round(videoHeight * (visible.height / croppedHeight));

        return new Placement(surfaceWidth, surfaceHeight, visible.width, visible.height);
    }

    private VideoFit() {}
}
