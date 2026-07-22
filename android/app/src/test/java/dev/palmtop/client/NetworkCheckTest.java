package dev.palmtop.client;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/**
 * Pins the address reasoning behind "why can this phone not reach the laptop".
 *
 * The concrete numbers here are from a real report: a phone that could not
 * connect, where the addresses alone contained the whole answer and nobody
 * could see it.
 */
public class NetworkCheckTest {

    private static byte[] ip(int a, int b, int c, int d) {
        return new byte[] { (byte) a, (byte) b, (byte) c, (byte) d };
    }

    @Test
    public void theReportedFailureIsCorrectlySeenAsDifferentNetworks() {
        // Phone on carrier NAT, laptop on a private LAN: unreachable, and the
        // reason the connect timed out rather than being refused.
        assertFalse(NetworkCheck.sameSubnet(ip(100, 97, 25, 122), ip(10, 136, 36, 186), 16));
        assertTrue("100.64.0.0/10 is carrier NAT -- i.e. mobile data, not Wi-Fi",
                NetworkCheck.isCarrierNat(ip(100, 97, 25, 122)));
    }

    @Test
    public void aStaleLaptopAddressOnTheSameLanIsStillADifferentSubnet() {
        // The subtler half of the same report: after the laptop moved networks
        // its old address stayed in the QR. Both are 10.x, so a naive
        // first-octet check would call them the same network and send the user
        // hunting for the wrong problem.
        assertFalse(NetworkCheck.sameSubnet(ip(10, 102, 108, 186), ip(10, 136, 36, 186), 24));
    }

    @Test
    public void aPhoneAndLaptopOnOneWifiAreSeenAsReachable() {
        assertTrue(NetworkCheck.sameSubnet(ip(192, 168, 1, 50), ip(192, 168, 1, 42), 24));
        assertFalse(NetworkCheck.isCarrierNat(ip(192, 168, 1, 50)));
    }

    @Test
    public void prefixLengthIsHonouredRatherThanAssumedToBe24() {
        // A /16 makes these the same network; a /24 does not. Getting this
        // wrong would produce confidently wrong advice.
        assertTrue(NetworkCheck.sameSubnet(ip(172, 20, 3, 9), ip(172, 20, 250, 4), 16));
        assertFalse(NetworkCheck.sameSubnet(ip(172, 20, 3, 9), ip(172, 20, 250, 4), 24));
    }

    @Test
    public void addressesJustOutsideCarrierNatAreNotFlagged() {
        // 100.64.0.0/10 spans 100.64 - 100.127 only.
        assertTrue(NetworkCheck.isCarrierNat(ip(100, 64, 0, 1)));
        assertTrue(NetworkCheck.isCarrierNat(ip(100, 127, 255, 254)));
        assertFalse(NetworkCheck.isCarrierNat(ip(100, 63, 0, 1)));
        assertFalse(NetworkCheck.isCarrierNat(ip(100, 128, 0, 1)));
    }
}
