package dev.palmtop.client;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * One laptop this phone has been paired with.
 *
 * <h3>Identity is the host's public key, not its address</h3>
 * A laptop's IP changes constantly -- joining a different network, moving
 * between a router and a phone hotspot, or simply a new DHCP lease. Keying
 * saved devices by address would therefore accumulate a duplicate entry for
 * every network the same machine was ever seen on, and every one of those
 * entries but the newest would silently fail to connect. This bit for real
 * during development: the host moved from 192.168.217.186 to 192.168.3.186
 * between two sessions and the stored address simply stopped working.
 *
 * The Noise static public key is the host's cryptographic identity: stable
 * across every address change, and already the thing the client pins to
 * decide the machine is who it claims to be. Keying on it means reconnecting
 * from a new network *updates* the saved entry rather than adding a broken
 * twin.
 *
 * JSON (de)serialisation lives here rather than in {@link DeviceStore} so it
 * can be unit-tested on a plain JVM without a {@code Context}, matching how
 * the rest of this codebase separates pure logic from Android plumbing.
 */
final class PairedDevice {
    final String name;
    final String host;
    final int port;
    final String token;
    /** Doubles as this device's stable identity -- see the class comment. */
    final String pubkey;
    /** Epoch millis, used only to order the list so the machine you actually
     *  use stays at the top. */
    final long lastConnectedAt;

    PairedDevice(String name, String host, int port, String token, String pubkey,
                 long lastConnectedAt) {
        this.name = name;
        this.host = host;
        this.port = port;
        this.token = token;
        this.pubkey = pubkey;
        this.lastConnectedAt = lastConnectedAt;
    }

    /** Two entries refer to the same laptop when their host keys match,
     *  regardless of what address either was last seen at. */
    boolean sameDeviceAs(PairedDevice other) {
        return other != null && pubkey != null && pubkey.equals(other.pubkey);
    }

    PairedDevice withLastConnectedNow() {
        return new PairedDevice(name, host, port, token, pubkey, System.currentTimeMillis());
    }

    /** Returns a copy reachable at a new address, keeping identity and
     *  pairing secret -- the normal case after the host changes networks. */
    PairedDevice movedTo(String newHost, int newPort) {
        return new PairedDevice(name, newHost, newPort, token, pubkey, lastConnectedAt);
    }

    boolean isUsable() {
        return host != null && !host.isEmpty()
                && port > 0
                && pubkey != null && !pubkey.isEmpty();
    }

    JSONObject toJson() throws JSONException {
        JSONObject o = new JSONObject();
        o.put("name", name == null ? "" : name);
        o.put("host", host == null ? "" : host);
        o.put("port", port);
        o.put("token", token == null ? "" : token);
        o.put("pubkey", pubkey == null ? "" : pubkey);
        o.put("lastConnectedAt", lastConnectedAt);
        return o;
    }

    static PairedDevice fromJson(JSONObject o) {
        return new PairedDevice(
                o.optString("name", "Palmtop host"),
                o.optString("host", ""),
                o.optInt("port", 0),
                o.optString("token", ""),
                o.optString("pubkey", ""),
                o.optLong("lastConnectedAt", 0L));
    }

    /** What the devices list shows under the device's name. */
    String subtitle() {
        return host + ":" + port;
    }

    @Override
    public String toString() {
        return name + " (" + subtitle() + ")";
    }
}
