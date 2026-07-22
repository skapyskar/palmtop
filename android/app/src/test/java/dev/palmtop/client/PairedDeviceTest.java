package dev.palmtop.client;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.json.JSONObject;
import org.junit.Test;

/**
 * Identity and serialisation for saved laptops. Worth real tests because the
 * identity rule (key on public key, not address) is the thing preventing a
 * failure that already happened once during development: the host changed
 * networks and its stored address silently stopped working.
 */
public class PairedDeviceTest {

    private static PairedDevice device(String host, int port, String pubkey) {
        return new PairedDevice("archlinux", host, port, "tok", pubkey, 1000L);
    }

    @Test
    public void sameHostKeyMeansSameDeviceEvenAtADifferentAddress() {
        PairedDevice before = device("192.168.217.186", 9999, "abc123");
        PairedDevice after = device("192.168.3.186", 9999, "abc123");
        assertTrue("a laptop that changed networks is still the same laptop",
                before.sameDeviceAs(after));
    }

    @Test
    public void differentHostKeysAreDifferentDevicesEvenAtTheSameAddress() {
        // Two different laptops can genuinely occupy the same address at
        // different times (DHCP reuse), so the address must not imply identity.
        PairedDevice a = device("192.168.3.186", 9999, "aaa");
        PairedDevice b = device("192.168.3.186", 9999, "bbb");
        assertFalse(a.sameDeviceAs(b));
    }

    @Test
    public void movedToKeepsIdentityAndSecretButChangesAddress() {
        PairedDevice original = device("192.168.217.186", 9999, "abc123");
        PairedDevice moved = original.movedTo("10.0.0.5", 1234);
        assertEquals("10.0.0.5", moved.host);
        assertEquals(1234, moved.port);
        assertEquals(original.pubkey, moved.pubkey);
        assertEquals(original.token, moved.token);
        assertTrue(original.sameDeviceAs(moved));
    }

    @Test
    public void jsonRoundTripPreservesEveryField() {
        PairedDevice original = new PairedDevice(
                "my-laptop", "192.168.1.5", 9999, "secrettoken", "deadbeef", 123456789L);
        PairedDevice restored = PairedDevice.fromJson(jsonOf(original));
        assertEquals(original.name, restored.name);
        assertEquals(original.host, restored.host);
        assertEquals(original.port, restored.port);
        assertEquals(original.token, restored.token);
        assertEquals(original.pubkey, restored.pubkey);
        assertEquals(original.lastConnectedAt, restored.lastConnectedAt);
    }

    @Test
    public void incompleteDevicesAreRejectedAsUnusable() {
        assertFalse("no address", device("", 9999, "abc").isUsable());
        assertFalse("no port", device("1.2.3.4", 0, "abc").isUsable());
        assertFalse("no host key -- could not be authenticated or identified",
                device("1.2.3.4", 9999, "").isUsable());
        assertTrue(device("1.2.3.4", 9999, "abc").isUsable());
    }

    @Test
    public void malformedJsonDegradesInsteadOfThrowing() {
        // Reading a partial entry must not throw: the caller's recovery path
        // is to skip unusable entries, which it can only do if it gets one.
        PairedDevice d = PairedDevice.fromJson(new JSONObject());
        assertFalse(d.isUsable());
    }

    @Test
    public void touchingADeviceMovesItsTimestampForward() throws Exception {
        PairedDevice original = device("1.2.3.4", 9999, "abc");
        Thread.sleep(2);
        PairedDevice touched = original.withLastConnectedNow();
        assertTrue(touched.lastConnectedAt > original.lastConnectedAt);
        assertTrue(original.sameDeviceAs(touched));
    }

    private static JSONObject jsonOf(PairedDevice d) {
        try {
            return d.toJson();
        } catch (Exception e) {
            throw new AssertionError(e);
        }
    }
}
