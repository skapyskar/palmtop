package dev.palmtop.spike;

/**
 * Quality-preset identifiers shared between the UI (mode picker, HUD) and
 * connection persistence (see {@link ConnectionState}).
 *
 * What each preset actually *does* -- resolution, fps, bitrate, GOP, drop
 * budget -- is defined exactly once, on the host (palmtopd's modes.rs), and
 * arrives over the wire in {@code VideoConfig}. This class only needs to
 * agree with the host on the wire discriminants and hold a display name for
 * each; it must never grow the actual preset values, or the two ends could
 * quietly disagree about what "Sync mode" means.
 */
final class Modes {
    static final int SYNC = 0;
    static final int BALANCED = 1;
    static final int QUALITY = 2;
    static final int BATTERY = 3;

    static final String[] NAMES = { "Sync", "Balanced", "Quality", "Battery" };

    static boolean isValid(int mode) {
        return mode >= 0 && mode < NAMES.length;
    }

    static String nameOf(int mode) {
        return isValid(mode) ? NAMES[mode] : "unknown(" + mode + ")";
    }

    private Modes() {}
}
