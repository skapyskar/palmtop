package dev.palmtop.client;

import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.os.BatteryManager;
import android.provider.Settings;
import android.util.Log;

/**
 * Reports whether this phone is ready for the laptop's USB pairing step.
 *
 * <h3>What this can and cannot know</h3>
 * USB debugging is a protocol by which a *computer* inspects a *device*. An
 * app running on the phone sits on the wrong end of it entirely: it cannot
 * enumerate ADB connections, cannot speak to the laptop across the cable, and
 * cannot detect that a laptop has detected it. Detection is something only
 * the laptop can do.
 *
 * So this class deliberately reports only the two preconditions the phone can
 * genuinely observe about itself -- is USB debugging switched on, and is a
 * cable attached -- and the app is honest that the laptop performs the rest.
 * That distinction is worth preserving rather than smoothing over: a user
 * staring at "waiting for laptop..." with no idea which side is at fault has
 * nothing to act on, whereas "USB debugging is OFF" is immediately fixable.
 */
final class UsbSetupStatus {
    private static final String TAG = "PalmtopClient";

    /**
     * Whether USB debugging is enabled in system settings.
     *
     * Readable without any permission, and genuinely useful: it is by far
     * the most common reason the laptop cannot see the phone at all.
     */
    static boolean isAdbEnabled(Context ctx) {
        try {
            return Settings.Global.getInt(ctx.getContentResolver(), Settings.Global.ADB_ENABLED, 0) == 1;
        } catch (Exception e) {
            Log.w(TAG, "could not read USB debugging state", e);
            return false;
        }
    }

    /**
     * Whether a USB cable is currently attached.
     *
     * Inferred from the battery's charging source, which is the only signal
     * available without privileged APIs. It cannot distinguish a data cable
     * from a charge-only one, so a "yes" here is necessary but not sufficient
     * -- reported as such rather than as a promise the connection will work.
     */
    static boolean isUsbConnected(Context ctx) {
        try {
            Intent battery = ctx.registerReceiver(null, new IntentFilter(Intent.ACTION_BATTERY_CHANGED));
            if (battery == null) return false;
            int plugged = battery.getIntExtra(BatteryManager.EXTRA_PLUGGED, -1);
            return plugged == BatteryManager.BATTERY_PLUGGED_USB;
        } catch (Exception e) {
            Log.w(TAG, "could not read USB connection state", e);
            return false;
        }
    }

    private UsbSetupStatus() {}
}
