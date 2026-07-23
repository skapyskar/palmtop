package dev.palmtop.client;

import android.content.Context;
import android.content.SharedPreferences;

/**
 * Everything needed to (re)connect to a palmtopd host, plus its persistence.
 *
 * Pulled out of {@code MainActivity} so "where a connection's details come
 * from and get saved" is one small, separately readable thing instead of
 * interleaved with view construction in {@code onCreate}.
 */
final class ConnectionState {
    private static final String PREFS = "palmtop";

    final String host;
    final int port;
    final String token;
    final String pubkey;
    final int mode;

    ConnectionState(String host, int port, String token, String pubkey, int mode) {
        this.host = host;
        this.port = port;
        this.token = token;
        this.pubkey = pubkey;
        this.mode = mode;
    }

    boolean hasHost() {
        return host != null && !host.isEmpty() && port != 0;
    }

    boolean hasPubkey() {
        return pubkey != null && !pubkey.isEmpty();
    }

    /**
     * Prefers the launching Intent's extras (a fresh adb-launched or QR-driven
     * connect), falling back to whatever was last persisted. Reopening via the
     * launcher icon sends a bare {@code ACTION_MAIN} Intent with no extras,
     * which otherwise meant every relaunch needed a fresh `adb shell am start`
     * from the host machine just to reconnect.
     *
     * A host present in the Intent is treated as authoritative and saved
     * immediately, overwriting whatever was persisted before.
     *
     * @param intentMode an explicit mode override (e.g. `--ei mode N`, which
     *     is what lets scripts/measure-latency.sh drive a run through all
     *     four presets unattended), or a negative number if the intent
     *     carried none.
     */
    static ConnectionState resolve(Context ctx, String intentHost, int intentPort,
                                   String intentToken, String intentPubkey, int intentMode) {
        SharedPreferences prefs = prefs(ctx);
        int mode = intentMode >= 0 ? intentMode : prefs.getInt("mode", Modes.BALANCED);
        if (!Modes.isValid(mode)) mode = Modes.BALANCED;

        boolean intentHasHost = intentHost != null && !intentHost.isEmpty() && intentPort != 0;
        if (!intentHasHost) {
            return new ConnectionState(
                    prefs.getString("host", null),
                    prefs.getInt("port", 0),
                    prefs.getString("token", ""),
                    prefs.getString("pubkey", ""),
                    mode);
        }

        ConnectionState fresh = new ConnectionState(
                intentHost, intentPort,
                intentToken == null ? "" : intentToken,
                intentPubkey == null ? "" : intentPubkey,
                mode);
        fresh.save(ctx);
        return fresh;
    }

    /** Returns a copy with different connection details, same mode -- used
     * once discovery/QR/manual entry settles on a host to connect to. */
    ConnectionState withHost(String host, int port, String token, String pubkey) {
        return new ConnectionState(host, port, token, pubkey, mode);
    }

    /** Returns a copy with a different mode, same connection details -- used
     * when the user picks a preset from the mode dialog. */
    ConnectionState withMode(int mode) {
        return new ConnectionState(host, port, token, pubkey, mode);
    }

    void save(Context ctx) {
        prefs(ctx).edit()
                .putString("host", host).putInt("port", port)
                .putString("token", token).putString("pubkey", pubkey)
                .putInt("mode", mode)
                .apply();
    }

    /**
     * The quality mode to start in: an explicit `--ei mode N` override if
     * the launching Intent carried one (which is what lets
     * scripts/measure-latency.sh drive a run through all four presets
     * unattended), otherwise whatever the user last chose.
     */
    static int resolveMode(Context ctx, int intentMode) {
        int mode = intentMode >= 0 ? intentMode : prefs(ctx).getInt("mode", Modes.BALANCED);
        return Modes.isValid(mode) ? mode : Modes.BALANCED;
    }

    static void saveMode(Context ctx, int mode) {
        prefs(ctx).edit().putInt("mode", mode).apply();
    }

    /**
     * Aspect ratio is a purely local rendering preference (see
     * {@link AspectMode}) -- it never crosses the wire, so it lives here
     * only for persistence, not as a field on {@link ConnectionState}
     * itself, which is otherwise entirely about what's needed to reconnect.
     */
    static int loadAspectMode(Context ctx) {
        int mode = prefs(ctx).getInt("aspectMode", AspectMode.BEST_FIT);
        return AspectMode.isValid(mode) ? mode : AspectMode.BEST_FIT;
    }

    static void saveAspectMode(Context ctx, int mode) {
        prefs(ctx).edit().putInt("aspectMode", mode).apply();
    }

    /**
     * Joystick speed at full deflection, in host pixels per second. Local
     * like the aspect ratio -- it shapes what this phone sends, and the host
     * neither knows nor needs to know about it.
     *
     * <p>Clamped on the way out as well as in, so a value written by an older
     * or newer build with a different range can never drive the cursor at a
     * speed this one considers unusable.
     */
    static float loadSensitivity(Context ctx) {
        return CursorDriver.clampSpeed(
                prefs(ctx).getFloat("sensitivity", CursorDriver.DEFAULT_SPEED_PX_S));
    }

    static void saveSensitivity(Context ctx, float pxPerSecond) {
        prefs(ctx).edit().putFloat("sensitivity", CursorDriver.clampSpeed(pxPerSecond)).apply();
    }

    private static SharedPreferences prefs(Context ctx) {
        return ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }
}
