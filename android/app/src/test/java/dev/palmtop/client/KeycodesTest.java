package dev.palmtop.client;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;

import org.junit.Test;

/**
 * The named evdev constants.
 *
 * <p>These numbers reach the compositor verbatim -- {@code input.rs} passes
 * them straight to {@code keyboard.key()} -- so a wrong value here is not a
 * compile error or a dropped event, it is a different key pressed on the
 * laptop. Cheap to pin, expensive to debug from the far end of a video
 * stream.
 *
 * <p>Values from {@code linux/input-event-codes.h}. The media keys were
 * additionally confirmed against the compiled "us" xkb keymap palmtopd
 * uploads, where evdev 115 appears as {@code <VOL+>} carrying
 * {@code XF86AudioRaiseVolume}.
 */
public class KeycodesTest {

    @Test
    public void editingKeysMatchEvdev() {
        assertEquals(1, Keycodes.KEY_ESC);
        assertEquals(14, Keycodes.KEY_BACKSPACE);
        assertEquals(15, Keycodes.KEY_TAB);
        assertEquals(28, Keycodes.KEY_ENTER);
        assertEquals(57, Keycodes.KEY_SPACE);
    }

    @Test
    public void modifierKeysMatchEvdev() {
        assertEquals(29, Keycodes.KEY_LEFTCTRL);
        assertEquals(42, Keycodes.KEY_LEFTSHIFT);
        assertEquals(56, Keycodes.KEY_LEFTALT);
        assertEquals(125, Keycodes.KEY_LEFTMETA);
    }

    @Test
    public void mediaKeysMatchEvdev() {
        assertEquals(113, Keycodes.KEY_MUTE);
        assertEquals(114, Keycodes.KEY_VOLUMEDOWN);
        assertEquals(115, Keycodes.KEY_VOLUMEUP);
    }

    @Test
    public void volumeKeysAreConsecutiveAndOrdered() {
        // Guards the easiest possible slip: swapping up and down, which is
        // silent, plausible, and maddening to notice over a video stream.
        assertEquals(Keycodes.KEY_MUTE + 1, Keycodes.KEY_VOLUMEDOWN);
        assertEquals(Keycodes.KEY_VOLUMEDOWN + 1, Keycodes.KEY_VOLUMEUP);
    }

    @Test
    public void asciiStillMapsAfterTheNewConstants() {
        // The letter/digit table lives in the same class; a botched edit to
        // the constants above could plausibly break its static initialiser.
        assertNotNull(Keycodes.lookup('a'));
        assertNotNull(Keycodes.lookup('Z'));
        assertNotNull(Keycodes.lookup('7'));
        assertEquals(Keycodes.KEY_SPACE, Keycodes.lookup(' ')[0]);
    }
}
