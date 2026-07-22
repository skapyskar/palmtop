package dev.palmtop.client;

/**
 * Client-local display presets for how the video is cropped/fit on screen.
 *
 * Unlike {@link Modes} (quality presets), these never touch the wire: the
 * host doesn't know or care how the phone chooses to display the frames it
 * already sent, so switching a ratio is instant, with no round trip.
 *
 * Modelled on how video players like VLC present an "aspect ratio" menu:
 * {@code BEST_FIT} shows the whole picture (never cropped, may letterbox);
 * every other preset crops the source to that shape, centered, so the result
 * fills more of the screen. None of these ever distort/stretch the picture --
 * only {@link VideoFit#computePlacement} decides how much to crop, always by
 * a uniform scale. A distorting "stretch to fill" mode was deliberately left
 * out: this app is for clicking precisely on a remote desktop, and a warped
 * picture makes judging where things are strictly harder, not more useful.
 */
final class AspectMode {
    static final int BEST_FIT = 0;
    static final int RATIO_16_9 = 1;
    static final int RATIO_4_3 = 2;
    static final int RATIO_1_1 = 3;

    static final String[] NAMES = { "Best Fit", "16:9", "4:3", "1:1" };

    static boolean isValid(int mode) {
        return mode >= 0 && mode < NAMES.length;
    }

    static String nameOf(int mode) {
        return isValid(mode) ? NAMES[mode] : "unknown(" + mode + ")";
    }

    /**
     * The target ratio for a preset, as {width, height}. {@code BEST_FIT} has
     * no fixed ratio of its own -- it always matches whatever the current
     * stream resolution is, which is what makes it "never crop" (a target
     * ratio equal to the source's own ratio needs no cropping at all).
     */
    static int[] ratioFor(int mode, int nativeWidth, int nativeHeight) {
        switch (mode) {
            case RATIO_16_9: return new int[] { 16, 9 };
            case RATIO_4_3:  return new int[] { 4, 3 };
            case RATIO_1_1:  return new int[] { 1, 1 };
            case BEST_FIT:
            default:         return new int[] { nativeWidth, nativeHeight };
        }
    }

    private AspectMode() {}
}
