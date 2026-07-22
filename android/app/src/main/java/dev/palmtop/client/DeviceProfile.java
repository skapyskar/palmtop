package dev.palmtop.client;

import android.app.Activity;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.os.Build;
import android.util.DisplayMetrics;
import android.util.Log;
import android.util.Range;
import android.view.Display;

/**
 * What this phone tells the host about itself, so the host can size the
 * stream correctly for hardware it has never seen.
 *
 * This replaces a real scaling problem rather than adding a nicety. The same
 * facts used to live in hand-written {@code config/devices/*.toml} files on
 * the host, filled in by running a probe script over ADB against each phone
 * -- fine for one developer with one phone on the desk, and impossible to
 * ship to strangers who have neither the script nor a reason to run it. The
 * phone already knows all of it; saying so at connect time removes the manual
 * step and means an unknown device is configured correctly on first
 * connection with nothing to edit.
 *
 * Everything here is queried defensively. These APIs are vendor-implemented
 * and genuinely do misbehave in the field, and a thrown exception during the
 * handshake would present to the user as "the app won't connect" with no
 * indication that a capability query was the cause. Any field that cannot be
 * determined falls back to a conservative value, which the host in turn
 * refuses to believe below its own credible floor -- so a bad reading costs
 * some quality, never a working connection.
 */
final class DeviceProfile {
    private static final String TAG = "PalmtopClient";

    final String model;
    final int screenWidth;
    final int screenHeight;
    final int densityDpi;
    final int refreshHz;
    final int maxDecodeWidth;
    final int maxDecodeHeight;
    final int maxDecodeFps;
    final boolean lowLatencyDecoder;

    private DeviceProfile(String model, int screenWidth, int screenHeight, int densityDpi,
                          int refreshHz, int maxDecodeWidth, int maxDecodeHeight,
                          int maxDecodeFps, boolean lowLatencyDecoder) {
        this.model = model;
        this.screenWidth = screenWidth;
        this.screenHeight = screenHeight;
        this.densityDpi = densityDpi;
        this.refreshHz = refreshHz;
        this.maxDecodeWidth = maxDecodeWidth;
        this.maxDecodeHeight = maxDecodeHeight;
        this.maxDecodeFps = maxDecodeFps;
        this.lowLatencyDecoder = lowLatencyDecoder;
    }

    /** Conservative stand-in, used when a query fails outright. Matches
     *  {@code DeviceProfile::conservative_default()} on the host. */
    static DeviceProfile fallback() {
        return new DeviceProfile("unknown", 1280, 720, 320, 60, 1280, 720, 30, false);
    }

    /**
     * Measures this device. Never throws -- see the class comment for why
     * that matters more than precision here.
     *
     * @param mime the video MIME type the host will actually send, since
     *     decode limits are per-codec and asking about the wrong one would
     *     produce confidently wrong numbers.
     */
    static DeviceProfile detect(Activity activity, String mime) {
        String model = safeModel();
        int[] screen = safeScreenSize(activity);
        int density = safeDensity(activity);
        int refresh = safeRefreshHz(activity);
        DecoderCapability decoder = safeDecoderCapability(mime);

        DeviceProfile profile = new DeviceProfile(
                model, screen[0], screen[1], density, refresh,
                decoder.maxWidth, decoder.maxHeight, decoder.maxFps, decoder.lowLatency);
        Log.i(TAG, "device profile: " + profile);
        return profile;
    }

    private static String safeModel() {
        try {
            String manufacturer = Build.MANUFACTURER == null ? "" : Build.MANUFACTURER.trim();
            String model = Build.MODEL == null ? "" : Build.MODEL.trim();
            // Vendors are inconsistent about whether MODEL already includes
            // the manufacturer ("moto g71 5G" does, "Pixel 9" does not), so
            // only prepend when it would not duplicate.
            if (!model.isEmpty() && !manufacturer.isEmpty()
                    && !model.toLowerCase().startsWith(manufacturer.toLowerCase())) {
                return manufacturer + " " + model;
            }
            if (!model.isEmpty()) return model;
            return manufacturer.isEmpty() ? "unknown" : manufacturer;
        } catch (Exception e) {
            return "unknown";
        }
    }

    /** @return {width, height} in the display's *natural* orientation. */
    private static int[] safeScreenSize(Activity activity) {
        try {
            DisplayMetrics metrics = new DisplayMetrics();
            activity.getWindowManager().getDefaultDisplay().getRealMetrics(metrics);
            if (metrics.widthPixels > 0 && metrics.heightPixels > 0) {
                return new int[] { metrics.widthPixels, metrics.heightPixels };
            }
        } catch (Exception e) {
            Log.w(TAG, "screen size query failed, using fallback", e);
        }
        return new int[] { 1280, 720 };
    }

    private static int safeDensity(Activity activity) {
        try {
            int dpi = activity.getResources().getDisplayMetrics().densityDpi;
            if (dpi > 0) return dpi;
        } catch (Exception e) {
            Log.w(TAG, "density query failed, using fallback", e);
        }
        return 320;
    }

    private static int safeRefreshHz(Activity activity) {
        try {
            Display display = activity.getWindowManager().getDefaultDisplay();
            float hz = display.getRefreshRate();
            // Rounded to whole Hz deliberately: panels report values like
            // 60.000004, and no decision made downstream has ever needed the
            // fractional part.
            if (hz >= 1f) return Math.round(hz);
        } catch (Exception e) {
            Log.w(TAG, "refresh rate query failed, using fallback", e);
        }
        return 60;
    }

    private static final class DecoderCapability {
        int maxWidth = 1280;
        int maxHeight = 720;
        int maxFps = 30;
        boolean lowLatency = false;
    }

    /**
     * Asks the platform what the best available hardware decoder for
     * {@code mime} can actually handle.
     *
     * Prefers a decoder advertising low latency, matching how the client
     * picks one at decode time -- reporting the limits of a decoder that
     * will not be the one used would be worse than reporting nothing.
     * Software decoders are skipped entirely: they can technically decode
     * large frames while being far too slow to do it in real time, so
     * believing their limits would produce a stream that plays at a
     * fraction of the intended rate.
     */
    private static DecoderCapability safeDecoderCapability(String mime) {
        DecoderCapability result = new DecoderCapability();
        try {
            MediaCodecList list = new MediaCodecList(MediaCodecList.REGULAR_CODECS);
            MediaCodecInfo best = null;
            boolean bestIsLowLatency = false;

            for (MediaCodecInfo info : list.getCodecInfos()) {
                if (info.isEncoder()) continue;
                boolean supportsMime = false;
                for (String type : info.getSupportedTypes()) {
                    if (type.equalsIgnoreCase(mime)) { supportsMime = true; break; }
                }
                if (!supportsMime) continue;

                String name = info.getName();
                if (name.startsWith("OMX.google") || name.startsWith("c2.android")) continue;

                boolean lowLatency = name.contains("low_latency");
                if (best == null || (lowLatency && !bestIsLowLatency)) {
                    best = info;
                    bestIsLowLatency = lowLatency;
                }
            }

            if (best == null) return result;
            result.lowLatency = bestIsLowLatency;

            MediaCodecInfo.VideoCapabilities video =
                    best.getCapabilitiesForType(mime).getVideoCapabilities();
            if (video == null) return result;

            Range<Integer> widths = video.getSupportedWidths();
            Range<Integer> heights = video.getSupportedHeights();
            result.maxWidth = widths.getUpper();
            result.maxHeight = heights.getUpper();

            // Frame rate has to be asked for *at the resolution we intend to
            // use*, not in the abstract: a decoder's headline maximum is
            // typically quoted for a small frame, and quoting it back for a
            // 1080p stream would license a rate the hardware cannot sustain.
            int probeWidth = Math.min(1920, result.maxWidth);
            int probeHeight = Math.min(1080, result.maxHeight);
            try {
                Range<Double> fps = video.getSupportedFrameRatesFor(probeWidth, probeHeight);
                result.maxFps = (int) Math.floor(fps.getUpper());
            } catch (Exception e) {
                // Thrown when the probe size is not supported at all; the
                // conservative default already covers that case.
                Log.w(TAG, "frame-rate query failed at " + probeWidth + "x" + probeHeight, e);
            }
        } catch (Exception e) {
            Log.w(TAG, "decoder capability query failed, using conservative defaults", e);
        }
        return result;
    }

    @Override
    public String toString() {
        return model + " " + screenWidth + "x" + screenHeight + "@" + refreshHz + "Hz"
                + " dpi=" + densityDpi
                + " decode<=" + maxDecodeWidth + "x" + maxDecodeHeight + "@" + maxDecodeFps
                + " lowLatency=" + lowLatencyDecoder;
    }
}
