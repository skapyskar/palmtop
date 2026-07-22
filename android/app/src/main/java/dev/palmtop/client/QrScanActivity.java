package dev.palmtop.client;

import android.Manifest;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.media.Image;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.SystemClock;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.os.VibratorManager;
import android.util.Log;
import android.util.Size;
import android.view.Gravity;
import android.view.MotionEvent;
import android.widget.FrameLayout;
import android.widget.TextView;

import androidx.activity.ComponentActivity;
import androidx.activity.result.ActivityResultLauncher;
import androidx.activity.result.contract.ActivityResultContracts;
import androidx.camera.core.Camera;
import androidx.camera.core.CameraControl;
import androidx.camera.core.CameraSelector;
import androidx.camera.core.ExperimentalGetImage;
import androidx.camera.core.FocusMeteringAction;
import androidx.camera.core.ImageAnalysis;
import androidx.camera.core.ImageProxy;
import androidx.camera.core.MeteringPoint;
import androidx.camera.core.Preview;
import androidx.camera.core.ZoomState;
import androidx.camera.core.resolutionselector.AspectRatioStrategy;
import androidx.camera.core.resolutionselector.ResolutionSelector;
import androidx.camera.core.resolutionselector.ResolutionStrategy;
import androidx.camera.lifecycle.ProcessCameraProvider;
import androidx.camera.view.PreviewView;
import androidx.core.content.ContextCompat;

import com.google.mlkit.vision.barcode.BarcodeScanner;
import com.google.mlkit.vision.barcode.BarcodeScannerOptions;
import com.google.mlkit.vision.barcode.BarcodeScanning;
import com.google.mlkit.vision.barcode.ZoomSuggestionOptions;
import com.google.mlkit.vision.barcode.common.Barcode;
import com.google.mlkit.vision.common.InputImage;

import java.util.List;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Scans the QR code palmtopd prints on startup -- a
 * {@code palmtop://host:port/token?pubkey=<hex>} URI (see
 * palmtopd/src/pairing.rs) -- and returns the parsed fields to whoever
 * launched this Activity via the classic {@code startActivityForResult}
 * API. Not the modern {@code registerForActivityResult} pattern on the
 * *caller* side deliberately: MainActivity is a plain {@code
 * android.app.Activity} (predates this feature needing a
 * {@code ComponentActivity} base), and changing its base class was a bigger,
 * riskier change than just using the older but still fully-supported result
 * API for this one launch site.
 *
 * This Activity itself extends {@code ComponentActivity} (not plain
 * {@code Activity}) because CameraX's {@code bindToLifecycle} requires a
 * {@code LifecycleOwner}, which only the AndroidX Activity provides.
 *
 * <h3>Why this is tuned the way it is</h3>
 * The first version of this screen detected <em>nothing</em> -- no errors, no
 * failures, a perfectly healthy camera pipeline, and zero barcodes across
 * repeated real attempts. The cause was CameraX's default ImageAnalysis
 * resolution of <b>640x480</b>. Our pairing URI carries a 64-hex-character
 * Noise public key, which pushes the QR to a high version with very small
 * modules, and palmtopd renders it in a terminal out of Unicode half-blocks --
 * so at 640x480, from any sane holding distance, the modules landed on well
 * under a pixel each and were simply unresolvable. Nothing in the logs said
 * so, because nothing was wrong; the decoder just never had the detail.
 *
 * Hence the three deliberate choices below, in descending order of how much
 * they mattered:
 * <ol>
 *   <li><b>{@link #ANALYSIS_TARGET} 1080p analysis frames.</b> The actual fix.
 *       Costs frame rate -- ML Kit at 1080p runs well under 30fps on a
 *       mid-range SoC -- but {@code STRATEGY_KEEP_ONLY_LATEST} means slow
 *       analysis just drops frames rather than queueing, and a handful of
 *       readable frames per second beats thirty unreadable ones.</li>
 *   <li><b>ML Kit's zoom suggestion.</b> When it sees a code that's present
 *       but too small to decode, it asks us to zoom, and we do -- so a user
 *       standing slightly too far back gets pulled in automatically instead of
 *       having to discover the right distance by trial and error.</li>
 *   <li><b>QR-only formats.</b> Narrows the decoder's search; also stops a
 *       stray barcode elsewhere on screen from being reported.</li>
 * </ol>
 * The live outline drawn by {@link QrOverlayView} exists for the same reason:
 * "detected but not decodable" and "not detected at all" looked identical
 * before, which is exactly what made the original bug so opaque.
 */
public class QrScanActivity extends ComponentActivity {
    private static final String TAG = "PalmtopQrScan";
    private static final String PALMTOP_SCHEME = "palmtop://";

    /**
     * Resolution requested for the analysis stream. 1080p is the top of
     * CameraX's guaranteed Preview+ImageAnalysis combination, so it binds on
     * essentially any device; CLOSEST_HIGHER_THEN_LOWER lets it settle for the
     * nearest thing if a device disagrees.
     */
    private static final Size ANALYSIS_TARGET = new Size(1920, 1080);

    /** How long the locked-on green outline stays visible before finishing. */
    private static final long LOCK_CONFIRM_MS = 320;

    /**
     * Ceiling on auto-zoom. Past roughly this, holding a code inside a
     * hand-held frame stops being realistic -- and it's digital zoom on this
     * class of device, so the extra magnification adds no real detail once
     * it's cropping past the sensor's native readout.
     */
    private static final float ZOOM_CAP = 4f;
    /** Fractional change below which a zoom suggestion is ignored. */
    private static final float ZOOM_DEADBAND = 0.25f;
    private static final long ZOOM_MIN_INTERVAL_MS = 700;

    private final ActivityResultLauncher<String> requestPermission =
            registerForActivityResult(new ActivityResultContracts.RequestPermission(), granted -> {
                if (granted) {
                    startCamera();
                } else {
                    finishWithError("Camera permission denied");
                }
            });

    private PreviewView previewView;
    private QrOverlayView overlayView;
    private TextView statusView;
    private ExecutorService analysisExecutor;

    /**
     * Built only once the camera is bound, because the zoom-suggestion option
     * needs the camera's real max zoom ratio. Read from the analysis thread.
     */
    private volatile BarcodeScanner scanner;
    private volatile CameraControl cameraControl;

    /** Zoom state, all touched only from ML Kit's zoom-suggestion callback. */
    private volatile float maxUsefulZoom = 1f;
    private volatile float appliedZoom = 1f;
    private volatile long lastZoomChangeAt = 0L;

    /** Barcode analysis runs continuously on a background thread; this stops
     * a second frame from racing to also finish() after a code is already
     * found and being acted on. */
    private final AtomicBoolean handled = new AtomicBoolean(false);
    private final AtomicBoolean loggedResolution = new AtomicBoolean(false);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        FrameLayout root = new FrameLayout(this);

        previewView = new PreviewView(this);
        // Explicit even though it's the default: QrOverlayView's coordinate
        // mapping reimplements exactly this scale type, so the two must not
        // drift apart silently.
        previewView.setScaleType(PreviewView.ScaleType.FILL_CENTER);
        root.addView(previewView, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        overlayView = new QrOverlayView(this);
        root.addView(overlayView, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        statusView = new TextView(this);
        statusView.setText("Point the camera at palmtopd's QR code — tap to focus");
        statusView.setTextColor(Color.WHITE);
        statusView.setBackgroundColor(Color.argb(160, 0, 0, 0));
        statusView.setTextSize(16);
        FrameLayout.LayoutParams statusLp = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT);
        statusLp.gravity = Gravity.TOP;
        root.addView(statusView, statusLp);
        setContentView(root);

        // Tap-to-focus. Continuous autofocus tends to hunt when pointed at a
        // flat, evenly-lit screen at close range, which is precisely our case.
        previewView.setOnTouchListener((v, event) -> {
            if (event.getActionMasked() == MotionEvent.ACTION_UP) {
                focusAt(event.getX(), event.getY());
                v.performClick();
            }
            return true;
        });

        analysisExecutor = Executors.newSingleThreadExecutor();

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA)
                == PackageManager.PERMISSION_GRANTED) {
            startCamera();
        } else {
            requestPermission.launch(Manifest.permission.CAMERA);
        }
    }

    private void startCamera() {
        var providerFuture = ProcessCameraProvider.getInstance(this);
        providerFuture.addListener(() -> {
            try {
                ProcessCameraProvider provider = providerFuture.get();

                // Pin both streams to the same aspect ratio so they share a
                // field of view -- QrOverlayView maps analysis-space corner
                // points onto the preview with a single transform, which is
                // only correct if the two aren't differently letterboxed.
                AspectRatioStrategy ratio = AspectRatioStrategy.RATIO_16_9_FALLBACK_AUTO_STRATEGY;

                Preview preview = new Preview.Builder()
                        .setResolutionSelector(new ResolutionSelector.Builder()
                                .setAspectRatioStrategy(ratio)
                                .build())
                        .build();
                preview.setSurfaceProvider(previewView.getSurfaceProvider());

                ImageAnalysis analysis = new ImageAnalysis.Builder()
                        .setResolutionSelector(new ResolutionSelector.Builder()
                                .setAspectRatioStrategy(ratio)
                                .setResolutionStrategy(new ResolutionStrategy(
                                        ANALYSIS_TARGET,
                                        ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER))
                                .build())
                        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                        .build();

                provider.unbindAll();
                Camera camera = provider.bindToLifecycle(
                        this, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis);
                cameraControl = camera.getCameraControl();

                float maxZoom = 1f;
                ZoomState zoom = camera.getCameraInfo().getZoomState().getValue();
                if (zoom != null) maxZoom = zoom.getMaxZoomRatio();
                // Tell ML Kit the device's real ceiling, but hold ourselves to
                // the lower usable one -- see applySuggestedZoom.
                maxUsefulZoom = Math.min(maxZoom, ZOOM_CAP);
                Log.i(TAG, "camera bound, max zoom " + maxZoom + "x (auto-zoom capped at "
                        + maxUsefulZoom + "x)");

                scanner = BarcodeScanning.getClient(new BarcodeScannerOptions.Builder()
                        .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
                        .setZoomSuggestionOptions(
                                new ZoomSuggestionOptions.Builder(this::applySuggestedZoom)
                                        .setMaxSupportedZoomRatio(maxZoom)
                                        .build())
                        .build());

                // Attached last, so no frame can reach a null scanner.
                analysis.setAnalyzer(analysisExecutor, this::analyzeFrame);
            } catch (Exception e) {
                Log.e(TAG, "camera setup failed", e);
                runOnUiThread(() -> finishWithError("Camera setup failed: " + e));
            }
        }, ContextCompat.getMainExecutor(this));
    }

    /**
     * ML Kit calls this when it can see a code but it's too small to decode.
     * Returning true tells it we actually applied the zoom, so it keeps
     * suggesting; returning false makes it stop asking.
     *
     * Applying every suggestion verbatim -- which is what the obvious
     * implementation does -- turned out to look broken in practice. Observed
     * live: 1.7x, 2.3x, 2.0x, 4.5x, 3.8x, 3.5x, 1.0x within twenty seconds.
     * The suggestions are derived from the code's apparent size in frame, so
     * while the code is blurry or partly out of frame that estimate swings
     * wildly, and the preview visibly pumps in and out. That actively fights
     * the user, who is being told to hold steady while the framing keeps
     * changing under them -- and at 4.5x, keeping a code in frame by hand is
     * genuinely hard.
     *
     * So: clamp, deadband, and rate-limit. None of this is load-bearing for a
     * successful scan (1080p analysis is what actually made scanning work);
     * it just stops a helper feature from making the shot harder to hold.
     */
    private boolean applySuggestedZoom(float zoomRatio) {
        CameraControl control = cameraControl;
        if (control == null) return false;

        float target = Math.max(1f, Math.min(zoomRatio, maxUsefulZoom));

        // Deadband: ignore small corrections entirely, so the preview settles.
        if (Math.abs(target - appliedZoom) < appliedZoom * ZOOM_DEADBAND) return true;
        // Rate limit: at most one change per interval, so a burst of wildly
        // different estimates can't turn into a burst of zoom changes.
        long now = SystemClock.elapsedRealtime();
        if (now - lastZoomChangeAt < ZOOM_MIN_INTERVAL_MS) return true;

        lastZoomChangeAt = now;
        appliedZoom = target;
        control.setZoomRatio(target);
        Log.i(TAG, String.format(Locale.US,
                "zoom %.2fx (ML Kit suggested %.2fx -- code visible but too small)",
                target, zoomRatio));
        runOnUiThread(() -> statusView.setText(
                String.format(Locale.US, "Zoomed to %.1f× — hold steady, tap to focus", target)));
        return true;
    }

    private void focusAt(float x, float y) {
        CameraControl control = cameraControl;
        if (control == null) return;
        MeteringPoint point = previewView.getMeteringPointFactory().createPoint(x, y);
        control.startFocusAndMetering(
                new FocusMeteringAction.Builder(point, FocusMeteringAction.FLAG_AF | FocusMeteringAction.FLAG_AE)
                        .setAutoCancelDuration(3, TimeUnit.SECONDS)
                        .build());
    }

    @ExperimentalGetImage
    private void analyzeFrame(ImageProxy imageProxy) {
        BarcodeScanner active = scanner;
        Image media = imageProxy.getImage();
        if (handled.get() || active == null || media == null) {
            imageProxy.close();
            return;
        }

        int rotation = imageProxy.getImageInfo().getRotationDegrees();
        // ML Kit reports corner points in the *rotated* frame's coordinate
        // space, so the overlay's source dimensions swap for 90/270.
        boolean swapped = rotation == 90 || rotation == 270;
        int srcW = swapped ? imageProxy.getHeight() : imageProxy.getWidth();
        int srcH = swapped ? imageProxy.getWidth() : imageProxy.getHeight();

        if (loggedResolution.compareAndSet(false, true)) {
            // The whole original bug was an unnoticed 640x480 here. Log it once
            // so the next person debugging a non-detecting scanner sees it
            // immediately instead of digging through CameraX's own logs.
            Log.i(TAG, "analysis frames are " + imageProxy.getWidth() + "x" + imageProxy.getHeight()
                    + " (rotation " + rotation + "deg -> " + srcW + "x" + srcH + " upright)");
        }

        InputImage image = InputImage.fromMediaImage(media, rotation);
        active.process(image)
                .addOnSuccessListener(barcodes -> onBarcodesFound(barcodes, srcW, srcH))
                .addOnFailureListener(e -> Log.w(TAG, "barcode scan failed", e))
                .addOnCompleteListener(task -> imageProxy.close());
    }

    private void onBarcodesFound(List<Barcode> barcodes, int srcW, int srcH) {
        if (handled.get()) return;

        if (barcodes.isEmpty()) {
            overlayView.clearDetection();
            return;
        }

        Barcode match = null;
        for (Barcode candidate : barcodes) {
            String raw = candidate.getRawValue();
            if (raw != null && raw.startsWith(PALMTOP_SCHEME)) {
                match = candidate;
                break;
            }
        }

        // Outline whatever we can see, even a code that isn't ours -- "wrong
        // code" and "no code" being indistinguishable is what made the
        // original failure so hard to diagnose.
        Barcode outlined = match != null ? match : barcodes.get(0);
        overlayView.setDetection(outlined.getCornerPoints(), srcW, srcH, match != null);

        if (match == null) {
            Log.i(TAG, "QR decoded but not a palmtop URI: " + describe(barcodes.get(0).getRawValue()));
            runOnUiThread(() -> statusView.setText(
                    "That's a QR code, but not palmtop's — scan the one palmtopd printed"));
            return;
        }

        Intent result;
        try {
            result = parsePairingUri(match.getRawValue());
        } catch (Exception e) {
            Log.w(TAG, "failed to parse scanned QR as a palmtop:// URI", e);
            runOnUiThread(() -> statusView.setText("Couldn't read that palmtop code — keep scanning"));
            return;
        }
        if (result == null) {
            runOnUiThread(() -> statusView.setText("Incomplete palmtop code — keep scanning"));
            return;
        }
        if (!handled.compareAndSet(false, true)) return; // another frame already won

        haptic();
        String summary = result.getStringExtra("host") + ":" + result.getIntExtra("port", 0);
        Log.i(TAG, "scanned pairing details for " + summary);
        setResult(RESULT_OK, result);
        runOnUiThread(() -> {
            statusView.setText("Found " + summary);
            // Let the green outline actually land before the screen changes,
            // so the scan reads as deliberate rather than as a glitch.
            previewView.postDelayed(this::finish, LOCK_CONFIRM_MS);
        });
    }

    /** @return the result Intent, or null if any required field was missing. */
    private Intent parsePairingUri(String raw) {
        Uri uri = Uri.parse(raw);
        String host = uri.getHost();
        int port = uri.getPort();
        String token = uri.getPath(); // "/<token>"
        if (token != null && token.startsWith("/")) token = token.substring(1);
        String pubkey = uri.getQueryParameter("pubkey");

        if (host == null || port <= 0 || token == null || token.isEmpty()
                || pubkey == null || pubkey.isEmpty()) {
            return null;
        }

        Intent result = new Intent();
        result.putExtra("host", host);
        result.putExtra("port", port);
        result.putExtra("token", token);
        result.putExtra("pubkey", pubkey);
        return result;
    }

    /** Truncated, so an arbitrary scanned payload can't flood the log. */
    private static String describe(String raw) {
        if (raw == null) return "<null>";
        return raw.length() <= 48 ? raw : raw.substring(0, 48) + "… (" + raw.length() + " chars)";
    }

    private void haptic() {
        try {
            Vibrator vibrator;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                VibratorManager manager =
                        (VibratorManager) getSystemService(Context.VIBRATOR_MANAGER_SERVICE);
                vibrator = manager == null ? null : manager.getDefaultVibrator();
            } else {
                vibrator = (Vibrator) getSystemService(Context.VIBRATOR_SERVICE);
            }
            if (vibrator != null) {
                vibrator.vibrate(VibrationEffect.createOneShot(35, VibrationEffect.DEFAULT_AMPLITUDE));
            }
        } catch (Exception e) {
            Log.d(TAG, "no haptic feedback available", e);
        }
    }

    private void finishWithError(String message) {
        statusView.setTextColor(Color.RED);
        statusView.setText(message);
        setResult(RESULT_CANCELED);
    }

    @Override
    protected void onDestroy() {
        if (analysisExecutor != null) analysisExecutor.shutdown();
        BarcodeScanner active = scanner;
        if (active != null) active.close();
        super.onDestroy();
    }
}
