package dev.palmtop.client;

import android.content.Context;
import android.content.SharedPreferences;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * The saved list of laptops this phone can connect to.
 *
 * Deliberately a plain JSON blob in {@code SharedPreferences} rather than a
 * database: this list is a handful of entries that are read once at startup
 * and written only when pairing changes. A database would be more machinery
 * than the problem has.
 *
 * All merge logic lives in {@link #upsert}, keyed on the host's public key --
 * see {@link PairedDevice} for why identity is the key rather than the
 * address.
 */
final class DeviceStore {
    private static final String TAG = "PalmtopClient";
    private static final String PREFS = "palmtop";
    private static final String KEY_DEVICES = "devices";

    static List<PairedDevice> load(Context ctx) {
        List<PairedDevice> out = new ArrayList<>();
        String raw = prefs(ctx).getString(KEY_DEVICES, null);
        if (raw == null || raw.isEmpty()) return out;
        try {
            JSONArray arr = new JSONArray(raw);
            for (int i = 0; i < arr.length(); i++) {
                PairedDevice d = PairedDevice.fromJson(arr.getJSONObject(i));
                if (d.isUsable()) out.add(d);
            }
        } catch (Exception e) {
            // A corrupt blob must not brick the app into an unusable state.
            // Returning empty means the user is shown the "add a device"
            // screen and can pair again, which is recoverable; throwing here
            // would crash on every launch with no way out.
            Log.w(TAG, "saved device list unreadable, starting empty", e);
            return new ArrayList<>();
        }
        // Most recently used first, so the machine actually in use stays at
        // the top of the list without the user curating anything.
        Collections.sort(out, (a, b) -> Long.compare(b.lastConnectedAt, a.lastConnectedAt));
        return out;
    }

    static void save(Context ctx, List<PairedDevice> devices) {
        JSONArray arr = new JSONArray();
        for (PairedDevice d : devices) {
            try {
                arr.put(d.toJson());
            } catch (Exception e) {
                Log.w(TAG, "skipping unserialisable device " + d, e);
            }
        }
        prefs(ctx).edit().putString(KEY_DEVICES, arr.toString()).apply();
    }

    /**
     * Adds a device, or updates the existing entry for the same laptop.
     *
     * Re-pairing a machine that moved networks replaces its address and
     * token in place instead of leaving a stale duplicate that would fail to
     * connect -- the reason identity is keyed on the public key.
     */
    static void upsert(Context ctx, PairedDevice device) {
        if (!device.isUsable()) {
            Log.w(TAG, "refusing to save incomplete device " + device);
            return;
        }
        List<PairedDevice> devices = load(ctx);
        for (int i = 0; i < devices.size(); i++) {
            if (devices.get(i).sameDeviceAs(device)) {
                devices.set(i, device);
                save(ctx, devices);
                return;
            }
        }
        devices.add(device);
        save(ctx, devices);
    }

    static void remove(Context ctx, PairedDevice device) {
        List<PairedDevice> devices = load(ctx);
        List<PairedDevice> kept = new ArrayList<>();
        for (PairedDevice d : devices) {
            if (!d.sameDeviceAs(device)) kept.add(d);
        }
        save(ctx, kept);
    }

    /** Marks a device as just-used, which is what keeps the list ordered. */
    static void touch(Context ctx, PairedDevice device) {
        upsert(ctx, device.withLastConnectedNow());
    }

    /** The device to auto-connect to on launch, or null when none is saved. */
    static PairedDevice mostRecent(Context ctx) {
        List<PairedDevice> devices = load(ctx);
        return devices.isEmpty() ? null : devices.get(0);
    }

    private static SharedPreferences prefs(Context ctx) {
        return ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    private DeviceStore() {}
}
