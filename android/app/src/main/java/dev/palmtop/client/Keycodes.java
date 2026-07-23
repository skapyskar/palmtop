package dev.palmtop.client;

import java.util.HashMap;
import java.util.Map;

/**
 * ASCII character -> (evdev keycode, needs-shift) for a US QWERTY layout,
 * matching the "us" xkb keymap `palmtopd` uploads to the compositor.
 *
 * The host only implements {@code Key} events (evdev codes), not the
 * {@code Text} message -- see palmtopd/src/input.rs, which logs and drops
 * Text as a documented gap (full Unicode needs a per-character keymap upload
 * or IME/compose path). Decomposing typed characters into Key events here on
 * the client is the matching half of that scope decision: it covers "type a
 * sentence" in US ASCII without either side needing the Unicode path yet.
 */
final class Keycodes {
    private Keycodes() {}

    // Linux evdev keycodes, from linux/input-event-codes.h.
    static final int KEY_ESC = 1;
    static final int KEY_BACKSPACE = 14;
    static final int KEY_TAB = 15;
    static final int KEY_ENTER = 28;
    static final int KEY_SPACE = 57;

    // Modifier keys, for the latching modifier bar -- see ModifierLatch.
    // These are pressed as real keys, not just signalled as a bitmask, which
    // is what makes Super+drag and a bare Super tap (the overview) work.
    static final int KEY_LEFTCTRL = 29;
    static final int KEY_LEFTSHIFT = 42;
    static final int KEY_LEFTALT = 56;
    static final int KEY_LEFTMETA = 125;

    // Media keys. These carry XF86AudioRaiseVolume / LowerVolume / Mute in
    // the "us" keymap palmtopd uploads (verified against the compiled
    // keymap: evdev 115 becomes xkb <VOL+> = 123). Whether pressing them
    // actually changes the volume is then up to the desktop's own bindings
    // -- GNOME and KDE bind these out of the box, but a bare wlroots
    // compositor such as Hyprland or Sway only does if its config says so.
    static final int KEY_MUTE = 113;
    static final int KEY_VOLUMEDOWN = 114;
    static final int KEY_VOLUMEUP = 115;

    private static final Map<Character, int[]> MAP = new HashMap<>(); // {evdevCode, needsShift}

    static {
        String letters = "abcdefghijklmnopqrstuvwxyz";
        int[] letterCodes = {30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44};
        for (int i = 0; i < letters.length(); i++) {
            put(letters.charAt(i), letterCodes[i], false);
            put(Character.toUpperCase(letters.charAt(i)), letterCodes[i], true);
        }

        String digits = "1234567890";
        int[] digitCodes = {2, 3, 4, 5, 6, 7, 8, 9, 10, 11};
        String shiftedDigits = "!@#$%^&*()";
        for (int i = 0; i < digits.length(); i++) {
            put(digits.charAt(i), digitCodes[i], false);
            put(shiftedDigits.charAt(i), digitCodes[i], true);
        }

        put(' ', KEY_SPACE, false);
        put('\n', KEY_ENTER, false);
        put('\t', KEY_TAB, false);

        put('-', 12, false); put('_', 12, true);
        put('=', 13, false); put('+', 13, true);
        put('[', 26, false); put('{', 26, true);
        put(']', 27, false); put('}', 27, true);
        put(';', 39, false); put(':', 39, true);
        put('\'', 40, false); put('"', 40, true);
        put('`', 41, false); put('~', 41, true);
        put('\\', 43, false); put('|', 43, true);
        put(',', 51, false); put('<', 51, true);
        put('.', 52, false); put('>', 52, true);
        put('/', 53, false); put('?', 53, true);
    }

    private static void put(char c, int code, boolean shift) {
        MAP.put(c, new int[]{code, shift ? 1 : 0});
    }

    /** Returns {@code null} if the character has no known US-layout mapping. */
    static int[] lookup(char c) {
        return MAP.get(c);
    }
}
