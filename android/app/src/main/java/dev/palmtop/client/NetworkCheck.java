package dev.palmtop.client;

import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.InterfaceAddress;
import java.net.NetworkInterface;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Explains a failed connection in terms of where the two devices actually are
 * on the network.
 *
 * <p>"failed to connect ... after 10000ms" is true and useless. By far the
 * most common cause is not a bug at either end: the phone and the laptop are
 * simply not on the same network, and no amount of retrying will change that.
 * The phone already knows its own addresses and the address it was dialling,
 * so it can say which of the two is the problem instead of making the user
 * guess.
 *
 * <p>A real report that motivated this: a phone on {@code 100.97.25.122}
 * dialling a laptop on {@code 10.136.36.186}. Those cannot reach each other,
 * and the giveaway -- 100.64.0.0/10 being carrier NAT, i.e. mobile data
 * rather than Wi-Fi -- is not something a user should be expected to
 * recognise from an IP address.
 */
final class NetworkCheck {

    private NetworkCheck() {}

    /**
     * @return a human-readable explanation of why {@code targetIp} may be
     *     unreachable, or null if this phone does appear to be on the same
     *     network (in which case the cause is something else and a wrong
     *     guess would only mislead).
     */
    static String explainUnreachable(String targetIp) {
        try {
            InetAddress target = InetAddress.getByName(targetIp);
            if (!(target instanceof Inet4Address)) return null;
            byte[] targetBytes = target.getAddress();

            List<String> ours = new ArrayList<>();
            boolean sameNetwork = false;
            boolean onCarrierNat = false;

            for (NetworkInterface nif : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (!nif.isUp() || nif.isLoopback()) continue;
                for (InterfaceAddress ia : nif.getInterfaceAddresses()) {
                    InetAddress addr = ia.getAddress();
                    if (!(addr instanceof Inet4Address)) continue;
                    String text = addr.getHostAddress();
                    ours.add(text + "/" + ia.getNetworkPrefixLength() + " on " + nif.getName());
                    if (isCarrierNat(addr.getAddress())) onCarrierNat = true;
                    if (sameSubnet(addr.getAddress(), targetBytes, ia.getNetworkPrefixLength())) {
                        sameNetwork = true;
                    }
                }
            }

            if (ours.isEmpty()) {
                return "This phone has no network connection at all. Turn on Wi-Fi and join "
                        + "the same network as the laptop.";
            }
            if (sameNetwork) {
                // Same subnet but still unreachable: this is the AP-isolation
                // case, not a routing one, and saying "different networks"
                // here would send the user off in the wrong direction.
                return "This phone is on the same network as the laptop (" + join(ours) + "), "
                        + "so the two can see each other in principle. The connection was still "
                        + "refused or ignored, which usually means either the laptop's daemon is "
                        + "not running, or the Wi-Fi blocks devices from talking to each other "
                        + "(common on guest, cafe, hostel and university networks). Try the "
                        + "laptop's own hotspot, or run  palmtopd --doctor  on the laptop.";
            }

            StringBuilder sb = new StringBuilder();
            sb.append("This phone and the laptop are on different networks, so they cannot "
                    + "reach each other.\n")
              .append("  laptop: ").append(targetIp).append('\n')
              .append("  phone:  ").append(join(ours)).append('\n');
            if (onCarrierNat) {
                sb.append("This phone looks like it is on mobile data rather than Wi-Fi "
                        + "(100.64.x.x is carrier NAT). ");
            }
            sb.append("Join this phone to the same Wi-Fi the laptop is on. If the laptop is "
                    + "connected to another phone's hotspot, this phone has to join that same "
                    + "hotspot -- nothing outside it can reach the laptop.");
            return sb.toString();
        } catch (Exception e) {
            // Diagnostics must never become the reason a failure is reported
            // wrongly; saying nothing is better than saying something false.
            return null;
        }
    }

    /** 100.64.0.0/10, RFC 6598 -- carrier-grade NAT, typical of mobile data. */
    static boolean isCarrierNat(byte[] ip) {
        int first = ip[0] & 0xFF, second = ip[1] & 0xFF;
        return first == 100 && second >= 64 && second <= 127;
    }

    static boolean sameSubnet(byte[] a, byte[] b, int prefixLength) {
        if (prefixLength <= 0 || prefixLength > 32) return false;
        int bits = prefixLength;
        for (int i = 0; i < 4 && bits > 0; i++) {
            int take = Math.min(8, bits);
            int mask = (0xFF << (8 - take)) & 0xFF;
            if (((a[i] ^ b[i]) & mask) != 0) return false;
            bits -= take;
        }
        return true;
    }

    private static String join(List<String> items) {
        return String.join(", ", items);
    }
}
