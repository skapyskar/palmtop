package dev.palmtop.client;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** Latched modifier state -- pure logic, no Android. */
public class ModifierLatchTest {

    @Test
    public void startsWithNothingLatched() {
        ModifierLatch latch = new ModifierLatch();
        assertEquals(0, latch.mask());
        for (int mod : ModifierLatch.ALL) {
            assertFalse(latch.isLatched(mod));
        }
    }

    @Test
    public void toggleLatchesThenReleases() {
        ModifierLatch latch = new ModifierLatch();
        assertTrue("first toggle should latch", latch.toggle(Protocol.MOD_SUPER));
        assertTrue(latch.isLatched(Protocol.MOD_SUPER));
        assertFalse("second toggle should release", latch.toggle(Protocol.MOD_SUPER));
        assertFalse(latch.isLatched(Protocol.MOD_SUPER));
    }

    @Test
    public void modifiersCombine() {
        // Ctrl+Alt+Del territory: latching one must not disturb another.
        ModifierLatch latch = new ModifierLatch();
        latch.toggle(Protocol.MOD_CTRL);
        latch.toggle(Protocol.MOD_ALT);
        assertEquals(Protocol.MOD_CTRL | Protocol.MOD_ALT, latch.mask());
        assertTrue(latch.isLatched(Protocol.MOD_CTRL));
        assertTrue(latch.isLatched(Protocol.MOD_ALT));
        assertFalse(latch.isLatched(Protocol.MOD_SUPER));
    }

    @Test
    public void releasingOneLeavesTheOthers() {
        ModifierLatch latch = new ModifierLatch();
        latch.toggle(Protocol.MOD_CTRL);
        latch.toggle(Protocol.MOD_SHIFT);
        latch.toggle(Protocol.MOD_CTRL);
        assertEquals(Protocol.MOD_SHIFT, latch.mask());
    }

    @Test
    public void clearForgetsEverything() {
        ModifierLatch latch = new ModifierLatch();
        latch.toggle(Protocol.MOD_CTRL);
        latch.toggle(Protocol.MOD_SUPER);
        latch.clear();
        assertEquals(0, latch.mask());
    }

    @Test
    public void everyModifierMapsToADistinctKeycode() {
        // A duplicate here would mean two buttons fighting over one key --
        // latching Alt would silently release Ctrl on the host.
        java.util.Set<Integer> seen = new java.util.HashSet<>();
        for (int mod : ModifierLatch.ALL) {
            int code = ModifierLatch.keycodeFor(mod);
            assertNotEquals("no keycode for mod bit " + mod, -1, code);
            assertTrue("duplicate keycode " + code, seen.add(code));
        }
    }

    @Test
    public void keycodesMatchEvdev() {
        // From linux/input-event-codes.h -- these reach the compositor
        // verbatim, so a wrong number here is a wrong key on the laptop.
        assertEquals(29, ModifierLatch.keycodeFor(Protocol.MOD_CTRL));
        assertEquals(56, ModifierLatch.keycodeFor(Protocol.MOD_ALT));
        assertEquals(42, ModifierLatch.keycodeFor(Protocol.MOD_SHIFT));
        assertEquals(125, ModifierLatch.keycodeFor(Protocol.MOD_SUPER));
    }

    @Test
    public void unknownBitHasNoKeycode() {
        assertEquals(-1, ModifierLatch.keycodeFor(1 << 7));
    }

    @Test
    public void everyModifierHasALabel() {
        for (int mod : ModifierLatch.ALL) {
            assertNotEquals("?", ModifierLatch.labelFor(mod));
        }
    }
}
