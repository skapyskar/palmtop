package dev.palmtop.client;

/**
 * The latched state of Ctrl / Alt / Shift / Super.
 *
 * <h3>Why latching rather than press-and-hold</h3>
 * On a physical keyboard you hold Super and press D. That is not available
 * here: in landscape most Android IMEs go fullscreen and cover the app
 * entirely, so the modifier button is not on screen at the moment you type
 * the second key. A latch survives the keyboard opening over it, which
 * press-and-hold cannot.
 *
 * <h3>Why a real key press, not just a modifier bit</h3>
 * Each latch sends a genuine press of the modifier's own evdev key (and a
 * release when un-latched) rather than only OR-ing a bit into subsequent key
 * events. Three things fall out of that, none of which the bit alone gives:
 * <ul>
 *   <li>The compositor really holds the modifier, so <b>Super+drag</b> works
 *       -- pointer messages in this protocol carry no modifier field, so a
 *       bit-only latch could never affect a drag.</li>
 *   <li>Latching and un-latching with nothing typed in between sends a bare
 *       press-release, which is exactly what opens the GNOME overview or the
 *       KDE launcher. That is the single most common use of Super, and it is
 *       free here rather than needing a special case.</li>
 *   <li>Held modifiers behave the way the compositor already expects, rather
 *       than relying on every consumer honouring an out-of-band bitmask.</li>
 * </ul>
 *
 * <p>This class is pure state and lookup -- it sends nothing itself, so it is
 * testable on the JVM. {@code MainActivity} owns the wire.
 */
final class ModifierLatch {

    /** Every modifier this bar offers, in display order. */
    static final int[] ALL = {
        Protocol.MOD_CTRL, Protocol.MOD_ALT, Protocol.MOD_SHIFT, Protocol.MOD_SUPER,
    };

    private int mask;

    /** Currently latched modifiers, as a {@code Protocol.MOD_*} bitmask. */
    int mask() {
        return mask;
    }

    boolean isLatched(int modBit) {
        return (mask & modBit) != 0;
    }

    /**
     * Flips one modifier.
     *
     * @return true if it is now latched (so the caller sends a key press),
     *     false if it was just released (so the caller sends a key release)
     */
    boolean toggle(int modBit) {
        mask ^= modBit;
        return isLatched(modBit);
    }

    /** Forgets everything, without sending anything. The caller is
     *  responsible for releasing keys it has already pressed -- see
     *  {@code MainActivity.releaseLatchedModifiers()}. */
    void clear() {
        mask = 0;
    }

    /**
     * The evdev keycode for a modifier bit -- the left-hand variant, which is
     * what a single on-screen button should stand for.
     *
     * @return the keycode, or -1 for an unknown bit
     */
    static int keycodeFor(int modBit) {
        if (modBit == Protocol.MOD_CTRL) return Keycodes.KEY_LEFTCTRL;
        if (modBit == Protocol.MOD_ALT) return Keycodes.KEY_LEFTALT;
        if (modBit == Protocol.MOD_SHIFT) return Keycodes.KEY_LEFTSHIFT;
        if (modBit == Protocol.MOD_SUPER) return Keycodes.KEY_LEFTMETA;
        return -1;
    }

    /** The button label for a modifier bit. */
    static String labelFor(int modBit) {
        if (modBit == Protocol.MOD_CTRL) return "Ctrl";
        if (modBit == Protocol.MOD_ALT) return "Alt";
        if (modBit == Protocol.MOD_SHIFT) return "Shift";
        if (modBit == Protocol.MOD_SUPER) return "❖";
        return "?";
    }
}
