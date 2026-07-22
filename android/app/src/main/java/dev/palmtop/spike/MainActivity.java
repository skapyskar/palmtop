package dev.palmtop.spike;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.text.Editable;
import android.text.InputType;
import android.text.TextWatcher;
import android.util.Log;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.WindowManager;
import android.view.inputmethod.InputMethodManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.io.ByteArrayInputStream;
import java.io.DataInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.util.List;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;

/**
 * Palmtop client: connects to palmtopd, decodes the live video stream with a
 * low-latency hardware MediaCodec, and sends touch as direct absolute
 * pointer input (tap where you want to click, like a touchscreen -- not a
 * trackpad you drag a cursor around with) plus basic typing back over the
 * same connection.
 *
 * Direct-touch was chosen over trackpad-relative specifically because it
 * decouples input correctness from video round-trip latency: the video
 * feedback loop (capture -> encode -> network -> decode -> display) is real
 * and currently perceptible, but with absolute tap-to-click you never need
 * to *watch* a cursor arrive at your finger to know the click landed in the
 * right place -- position is set by where you touched, not by chasing a lag-
 * ging on-screen cursor. See palmtopd/src/input.rs's PointerMotionAbsolute
 * handler, proven independently by spike-capture-latency (20/20 detected).
 *
 * <h3>Layout: three regions, never overlapping</h3>
 * The screen is a top status/HUD bar, the video surface, and a bottom control
 * bar -- stacked in a {@code LinearLayout}, not overlaid corner-buttons on a
 * fullscreen video as an earlier version had. Two things follow from that:
 * <ol>
 *   <li>Controls can never be drawn over the laptop's screen, because they
 *       live in a structurally separate row, not a translucent layer above it.</li>
 *   <li>The video surface itself is sized to the laptop's real aspect ratio
 *       (see {@link VideoFit}) within whatever space the bars leave it, rather
 *       than stretched to fill the phone's screen regardless of shape.</li>
 * </ol>
 *
 * Evolved from the fixed-stream decode-latency spike (which proved the
 * MediaCodec low-latency + inflight-cap=1 approach at 25ms avg) into the real
 * client speaking palmtop-proto against the live palmtopd daemon, instead of
 * a canned test file.
 */
public class MainActivity extends Activity {
    private static final String TAG = "PalmtopClient";
    private static final String MIME = MediaFormat.MIMETYPE_VIDEO_AVC; // host only speaks h264 so far

    private String host;
    private int port;
    /** Pairing secret from the host's QR code -- see palmtopd/src/pairing.rs. */
    private String token = "";
    /** Host's static Noise public key (hex), for TOFU-pinning -- see
     * palmtop_proto::noise's doc comment for exactly what trusting a value
     * learned via mDNS/manual entry here does and doesn't protect against. */
    private String pubkey = "";
    /** Set once the Noise handshake completes; shared between the reader and
     * writer threads. Safe: NoiseTransport's crypto methods are `synchronized`
     * and never do I/O internally -- see its class doc comment for the
     * deadlock this design specifically avoids. */
    private volatile NoiseTransport noise;

    private SurfaceView surfaceView;
    private SurfaceHolder surfaceHolder;
    /** The video's actual home -- see {@link #buildVideoContainer()} and
     *  {@link #resizeSurfaceToFit}. */
    private FrameLayout videoContainer;
    /** The real crop boundary, sized to exactly the visible rect and
     *  centered within {@link #videoContainer} -- see
     *  {@link #buildVideoContainer()}'s doc comment for why this exists as
     *  a separate view from videoContainer itself. */
    private FrameLayout videoClip;
    private TextView statusView;
    private EditText hiddenInput;
    private Button kbToggle;
    private Button reconnectButton;
    private Button modeButton;
    private Button aspectButton;
    private Button hudToggle;
    private HudView hud;
    /** Turns 3+-finger touches into a local zoom/pan on {@link #surfaceView}
     *  -- see its class doc comment. Constructed once, alongside surfaceView,
     *  in {@link #buildVideoContainer()}. */
    private PinchZoomController pinchZoom;
    /** Current stream format. Replaced when the host announces a new one
     *  after a mode change, so it cannot be a local: the UI lambdas below
     *  would need it effectively-final. */
    private volatile Protocol.Received videoConfig;
    /** Preset the user picked. Persisted so a session resumed after an app
     *  restart comes back in the chosen mode rather than the host default. */
    private int currentMode = Modes.BALANCED;
    /** Local display preference -- see {@link AspectMode}. Never sent to the
     *  host; switching is instant, no round trip needed. */
    private int currentAspectMode = AspectMode.BEST_FIT;
    /** The video dimensions {@link #resizeSurfaceToFit} last computed a fit
     *  for -- re-supplied to it when {@link #videoContainer}'s own size
     *  changes (e.g. the first layout pass) rather than a VideoConfig. */
    private int knownVideoWidth = 0, knownVideoHeight = 0;

    /** Generation counter: incremented on every (re)connect so a network
     * thread from a *previous* connection attempt can tell it's stale and
     * exit quietly instead of fighting the new one over shared state. */
    private volatile int generation = 0;
    private volatile boolean connected = false;
    /** Visibility into the stale-frame-skip fix -- see
     * {@link #handleVideoFrame}. A healthy connection keeping up in real time
     * should show droppedFrames near zero; a growing ratio under sustained
     * high-motion content (e.g. video playback) means decode still can't
     * keep up with arrival, even after this fix stops it from compounding. */
    private long decodedFrames, droppedFrames;

    private Socket socket;
    private final LinkedBlockingQueue<byte[]> outbox = new LinkedBlockingQueue<>();

    private MediaCodec codec;
    private HandlerThread codecThread;
    private final LinkedBlockingQueue<Integer> availableInputs = new LinkedBlockingQueue<>();
    /** Caps frames in flight -- the change that took decode latency from ~40ms to 25ms
     * in the Phase 0 spike; same principle applied here from day one. */
    private final Semaphore inFlight = new Semaphore(1);

    /** Clock offset, RTT and latency percentiles. See LatencyTracker. */
    private final LatencyTracker latency = new LatencyTracker();
    /** How stale a frame may be before it is skipped. Set by the host's
     *  VideoConfig so the preset table has exactly one definition. */
    private volatile long dropBudgetUs = 80_000;
    /** When the single in-flight frame was queued to the decoder.
     *  Safe as one field only because inFlight caps frames in flight at 1
     *  -- if that cap is ever raised this must become a map keyed on
     *  presentation timestamp. */
    private volatile long queuedAtUs = 0;

    /** Probe schedule: a burst on connect, then steady state. At one per
     *  second the offset window would take ~15s to fill, and until it does
     *  every end-to-end figure rests on a barely-sampled offset. */
    private static final int PING_BURST = 5;
    private static final long PING_BURST_INTERVAL_US = 200_000;
    private static final long PING_STEADY_INTERVAL_US = 1_000_000;

    private static long nowUs() { return System.nanoTime() / 1000L; }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);

        // ConnectionState prefers the launching Intent's extras, falling back
        // to whatever was last persisted -- see its class doc comment for why
        // (reopening via the launcher icon sends a bare ACTION_MAIN Intent
        // with no extras).
        ConnectionState conn = ConnectionState.resolve(this,
                getIntent().getStringExtra("host"), getIntent().getIntExtra("port", 0),
                getIntent().getStringExtra("token"), getIntent().getStringExtra("pubkey"),
                getIntent().getIntExtra("mode", -1));
        host = conn.host;
        port = conn.port;
        token = conn.token;
        pubkey = conn.pubkey;
        currentMode = conn.mode;
        currentAspectMode = ConnectionState.loadAspectMode(this);

        FrameLayout root = buildUi();
        setContentView(root);

        if (!conn.hasHost() || !conn.hasPubkey()) {
            setControlsVisible(false);
            showDiscoveryOverlay(root);
            return;
        }

        wireSurfaceCallbacks();
    }

    // ------------------------------------------------------------ view construction

    /**
     * Builds the view hierarchy: the video surface in the middle, with a
     * control column on each side -- structurally separate regions,
     * specifically so the buttons can never end up drawn over the video, no
     * matter how the video's own size changes.
     *
     * The columns are {@code WRAP_CONTENT}, sized to their own button
     * content; the video is the one {@code weight=1} child and gets
     * whatever horizontal room that leaves. That is deliberate: it is
     * exactly "the space left over from maintaining the ratio" the columns
     * are meant to occupy, and -- because a {@code WRAP_CONTENT} sibling
     * never shrinks to zero on its own -- the columns keep their room even
     * when the video's aspect ratio happens to match the screen exactly and
     * there would otherwise be no natural pillarbox at all.
     */
    private FrameLayout buildUi() {
        FrameLayout root = new FrameLayout(this);

        LinearLayout mainLayout = new LinearLayout(this);
        mainLayout.setOrientation(LinearLayout.HORIZONTAL);
        root.addView(mainLayout, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        // A fixed pixel width, not WRAP_CONTENT -- found live that WRAP_CONTENT
        // does not mix safely with a `0dp + weight` sibling here. Diagnosed by
        // logging every child's measured size: the bar itself measured to
        // just its own padding (20x828) and *every* child inside it -- text
        // view, all five buttons, even the spacer -- measured to width 0,
        // heights all correct. That is LinearLayout handing the WRAP_CONTENT
        // sibling a near-zero width constraint before it can size itself from
        // its own content, a known category of bug with weighted siblings.
        // An explicit width sidesteps the whole mechanism rather than
        // depending on it. Generous enough for the longest realistic label
        // ("⚙ Balanced"/"▭ Best Fit"); anything longer wraps rather than
        // clips, which is an acceptable, non-broken fallback.
        int controlColumnWidth = (int) (170 * getResources().getDisplayMetrics().density);

        // Wrapped in a ScrollView rather than added directly: on a short
        // screen, or once enough controls accumulate, the column can need
        // more height than the video area actually offers it (MATCH_PARENT,
        // set by the video's own height). Without this, that overflow would
        // just clip the lowest buttons off invisibly. ScrollView requires
        // its direct child to be WRAP_CONTENT height, not MATCH_PARENT --
        // that is what lets the child legitimately be *taller* than the
        // viewport in the first place, which is the entire point.
        ScrollView leftScroll = new ScrollView(this);
        leftScroll.addView(buildLeftBar(), new ScrollView.LayoutParams(
                ScrollView.LayoutParams.MATCH_PARENT, ScrollView.LayoutParams.WRAP_CONTENT));
        mainLayout.addView(leftScroll, new LinearLayout.LayoutParams(
                controlColumnWidth, LinearLayout.LayoutParams.MATCH_PARENT));

        mainLayout.addView(buildVideoContainer(), new LinearLayout.LayoutParams(
                0, LinearLayout.LayoutParams.MATCH_PARENT, 1f));

        buildHiddenInput(root);
        return root;
    }

    /** Every control lives in this one column: connection status/reconnect,
     * live stats, quality mode, aspect ratio, HUD toggle, keyboard toggle. */
    private LinearLayout buildLeftBar() {
        LinearLayout bar = new LinearLayout(this);
        bar.setOrientation(LinearLayout.VERTICAL);
        bar.setBackgroundColor(Color.argb(160, 0, 0, 0));
        int pad = (int) (4 * getResources().getDisplayMetrics().density);
        bar.setPadding(pad, pad, pad, pad);

        statusView = new TextView(this);
        statusView.setTextColor(Color.GREEN);
        statusView.setTextSize(11);
        bar.addView(statusView);

        // Always-available escape hatch: retry the connection in place
        // (network hiccup, host restarted, portal dialog dismissed by
        // mistake, ...) without needing another adb-launched Intent.
        reconnectButton = new Button(this);
        reconnectButton.setText("⟳ Reconnect");
        reconnectButton.setOnClickListener(v -> startConnection());
        bar.addView(reconnectButton);

        hud = new HudView(this);
        bar.addView(hud);

        modeButton = new Button(this);
        modeButton.setOnClickListener(v -> showModePicker());
        bar.addView(modeButton);
        updateModeButton();

        aspectButton = new Button(this);
        aspectButton.setOnClickListener(v -> showAspectPicker());
        bar.addView(aspectButton);
        updateAspectButton();

        hudToggle = new Button(this);
        hudToggle.setText("📊");
        hudToggle.setOnClickListener(v -> hud.setShown(!hud.isHudShown()));
        bar.addView(hudToggle);

        kbToggle = new Button(this);
        kbToggle.setText("⌨");
        kbToggle.setOnClickListener(v -> showKeyboard());
        bar.addView(kbToggle);

        return bar;
    }

    /**
     * The video's actual home. Two nested layers, not one, and the split is
     * load-bearing -- found live, the hard way, when switching aspect ratios
     * had *no visible effect at all*.
     *
     * <p>The original (broken) design put {@link #surfaceView} directly in
     * {@code videoContainer} and relied on the container's ordinary child
     * clipping to crop an oversized surface down to the intended shape. That
     * fails whenever {@code videoContainer} itself is larger than the
     * *visible* rect -- which is whenever there is any letterbox/pillarbox
     * bar at all, i.e. almost always. Worse, it fails silently and often
     * invisibly: for a container bound by height, the algebra for the
     * "oversized" surface width collapses to exactly the *original*,
     * uncropped Best-Fit width regardless of the target ratio (the ratio
     * cancels out completely) -- so the surface was never actually larger
     * than the container to begin with, and there was nothing for clipping
     * to clip. Every aspect mode rendered identically.
     *
     * <p>{@link #videoClip} is the fix: a wrapper sized to *exactly* the
     * visible rect ({@link VideoFit.Placement#visibleWidth}/
     * {@code visibleHeight}), centered inside {@code videoContainer}. Only
     * *its* bounds -- not the container's -- are what the oversized,
     * centered {@code surfaceView} actually gets clipped against, so the
     * crop holds regardless of how much extra letterbox space the container
     * itself has. See {@link #resizeSurfaceToFit} for where both layers get
     * resized together, and {@link #wireSurfaceCallbacks} for why the touch
     * listener lives on {@code videoClip}, not {@code videoContainer}.
     *
     * <p>Black background on the container so any letterbox/pillarbox/crop
     * bars read as intentional, not as a rendering glitch. {@code
     * clipChildren} stated explicitly on both layers (it is the ViewGroup
     * default) because it is exactly the mechanism this depends on.
     */
    private FrameLayout buildVideoContainer() {
        videoContainer = new FrameLayout(this);
        videoContainer.setBackgroundColor(Color.BLACK);
        videoContainer.setClipChildren(true);

        videoClip = new FrameLayout(this);
        videoClip.setClipChildren(true);
        videoContainer.addView(videoClip, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        surfaceView = new SurfaceView(this);
        videoClip.addView(surfaceView, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        pinchZoom = new PinchZoomController(surfaceView);

        wireVideoContainerResize();
        return videoContainer;
    }

    /**
     * Re-fits the video surface whenever this container's own size settles --
     * covers the first layout pass (the container has no size at all until
     * then) and, defensively, any later one. Does not loop: changing
     * {@link #surfaceView}'s LayoutParams changes its own bounds, not
     * {@link #videoContainer}'s, so it cannot re-trigger this listener.
     */
    private void wireVideoContainerResize() {
        videoContainer.addOnLayoutChangeListener((v, left, top, right, bottom, oldLeft, oldTop, oldRight, oldBottom) -> {
            if (right - left != oldRight - oldLeft || bottom - top != oldBottom - oldTop) {
                resizeSurfaceToFit(knownVideoWidth, knownVideoHeight);
            }
        });
    }

    /**
     * Sizes {@link #surfaceView} to the largest rectangle that fits inside
     * {@link #videoContainer}, cropped to the currently selected
     * {@link AspectMode} -- see {@link VideoFit#computePlacement} for the
     * geometry. This is also the fix for the video being stretched to fill
     * the phone's screen regardless of the laptop's actual shape: Best Fit
     * (the default) simply never crops.
     *
     * Called whenever any input to that placement changes: a new/changed
     * VideoConfig (the caller supplies the new video dimensions directly),
     * the container's own size settling (the layout-change listener in
     * {@link #wireVideoContainerResize} re-supplies whatever dimensions were
     * last known), or the user picking a different aspect ratio
     * ({@link #selectAspectMode}).
     *
     * A no-op until both dimensions are actually known (video size 0 before
     * the first VideoConfig arrives, container size 0 before the first
     * layout pass) -- there is nothing sane to compute yet.
     */
    private void resizeSurfaceToFit(int videoWidth, int videoHeight) {
        knownVideoWidth = videoWidth;
        knownVideoHeight = videoHeight;
        int containerWidth = videoContainer.getWidth();
        int containerHeight = videoContainer.getHeight();
        if (containerWidth == 0 || containerHeight == 0 || videoWidth <= 0 || videoHeight <= 0) return;

        int[] ratio = AspectMode.ratioFor(currentAspectMode, videoWidth, videoHeight);
        VideoFit.Placement placement = VideoFit.computePlacement(
                containerWidth, containerHeight, videoWidth, videoHeight, ratio[0], ratio[1]);

        // The crop boundary -- sized to exactly the visible rect, not the
        // (usually larger) container. This is the layer whose bounds the
        // oversized surfaceView actually gets clipped against; see
        // buildVideoContainer()'s doc comment for the bug this fixes.
        FrameLayout.LayoutParams clipLp = (FrameLayout.LayoutParams) videoClip.getLayoutParams();
        clipLp.width = placement.visibleWidth;
        clipLp.height = placement.visibleHeight;
        clipLp.gravity = Gravity.CENTER;
        videoClip.setLayoutParams(clipLp);

        FrameLayout.LayoutParams lp = (FrameLayout.LayoutParams) surfaceView.getLayoutParams();
        lp.width = placement.surfaceWidth;
        lp.height = placement.surfaceHeight;
        lp.gravity = Gravity.CENTER;
        surfaceView.setLayoutParams(lp);

        // A new base placement invalidates any in-progress interactive
        // zoom/pan -- the coordinate system it was measured against no
        // longer exists.
        pinchZoom.reset();
        pinchZoom.setContentSize(placement.surfaceWidth, placement.surfaceHeight,
                placement.visibleWidth, placement.visibleHeight);
    }

    /**
     * Invisible-but-focusable EditText: the simplest reliable way to
     * capture typed text regardless of whether a given IME dispatches
     * discrete KeyEvents or batches via commitText (many do the latter,
     * especially with autocorrect) -- diffing the text content works
     * either way. See Keycodes.java for why this stays ASCII-only for now.
     */
    private void buildHiddenInput(FrameLayout root) {
        hiddenInput = new EditText(this);
        hiddenInput.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        root.addView(hiddenInput, new FrameLayout.LayoutParams(1, 1));
        hiddenInput.addTextChangedListener(new TextWatcher() {
            @Override public void beforeTextChanged(CharSequence s, int start, int count, int after) {}
            @Override public void onTextChanged(CharSequence s, int start, int before, int count) {
                // `before` chars at `start` were replaced by `count` new
                // chars. Diffing this way (instead of clearing the field
                // after every keystroke, which an earlier version did) is
                // what makes backspace work at all: clearing left the field
                // empty, and backspace on an *already-empty* field is a
                // no-op with nothing to delete -- no text change, so
                // afterTextChanged never fired and the key press vanished
                // silently. `before` catches a deletion regardless of what
                // (if anything) is left in the field afterwards.
                for (int i = 0; i < before; i++) {
                    sendChar('\b');
                }
                for (int i = 0; i < count; i++) {
                    sendChar(s.charAt(start + i));
                }
            }
            @Override public void afterTextChanged(Editable s) {}
        });
    }

    /** Toggles every interactive control at once -- used to hide them
     * entirely while the discovery overlay covers the screen, and show them
     * again once a connection target is chosen. GONE rather than just
     * visually covered: a previous FrameLayout z-order/sizing quirk let
     * corner-anchored buttons peek through underneath the overlay; GONE is
     * unambiguous regardless of the overlay's exact measured bounds. */
    private void setControlsVisible(boolean visible) {
        int v = visible ? View.VISIBLE : View.GONE;
        kbToggle.setVisibility(v);
        reconnectButton.setVisibility(v);
        modeButton.setVisibility(v);
        aspectButton.setVisibility(v);
        hudToggle.setVisibility(v);
    }

    /** Wires touch input and surface lifecycle -- shared by the direct-connect
     * path (host already known at launch, from {@link #onCreate}) and
     * {@link #completeConnectionSetup} (host chosen via discovery/QR/manual
     * entry), which previously duplicated this identically.
     *
     * The touch listener is on {@link #videoClip}, not {@link #surfaceView}
     * -- deliberately. surfaceView's own scale/translation change
     * continuously during a pinch-zoom gesture, and reading coordinates from
     * a view while that view's own transform is being live-edited is exactly
     * the kind of self-referential timing {@link #onTouch}'s manual
     * coordinate mapping is written to avoid, rather than depend on.
     * videoClip is never transformed, so touch coordinates read from it are
     * always stable, raw numbers -- and since videoClip is sized to exactly
     * the visible crop rect (see {@link #buildVideoContainer()}), it also
     * has no letterbox bars of its own for a touch to land in; a tap outside
     * the actual video content never reaches this listener at all. */
    private void wireSurfaceCallbacks() {
        videoClip.setOnTouchListener(this::onTouch);
        surfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override public void surfaceCreated(SurfaceHolder holder) {
                surfaceHolder = holder;
                startConnection();
            }
            @Override public void surfaceChanged(SurfaceHolder h, int f, int w, int ht) {}
            @Override public void surfaceDestroyed(SurfaceHolder h) { generation++; }
        });
    }

    // ------------------------------------------------------------ discovery

    private HostDiscovery hostDiscovery;

    /** Stashed so {@link #onActivityResult} (fired after QrScanActivity
     * returns) can tear down the same discovery views the manual/mDNS paths
     * do, via completeConnectionSetup. */
    private FrameLayout discoveryRoot;
    private LinearLayout discoveryOverlayView;

    private static final int REQUEST_SCAN_QR = 1;

    /**
     * Shown instead of the video surface when no host is known yet (first
     * launch with no adb-passed extras and nothing in SharedPreferences).
     * Lists palmtopd instances found via mDNS (HostDiscovery / NsdManager),
     * matching what palmtopd actually advertises -- see pairing.rs. Tapping
     * one, scanning a QR code, or filling in the manual-entry form all lead
     * to the same {@link #completeConnectionSetup}.
     */
    private void showDiscoveryOverlay(FrameLayout root) {
        LinearLayout overlay = new LinearLayout(this);
        overlay.setOrientation(LinearLayout.VERTICAL);
        overlay.setBackgroundColor(Color.BLACK);
        int pad = (int) (16 * getResources().getDisplayMetrics().density);
        overlay.setPadding(pad, pad, pad, pad);
        discoveryRoot = root;
        discoveryOverlayView = overlay;

        TextView title = new TextView(this);
        title.setText("Find a Palmtop host");
        title.setTextColor(Color.WHITE);
        title.setTextSize(20);
        overlay.addView(title);

        Button scanQrButton = new Button(this);
        scanQrButton.setText("📷 Scan QR code");
        scanQrButton.setOnClickListener(v ->
                startActivityForResult(new Intent(this, QrScanActivity.class), REQUEST_SCAN_QR));
        overlay.addView(scanQrButton);

        TextView hint = new TextView(this);
        hint.setText("Scanning for palmtopd on this Wi-Fi network...");
        hint.setTextColor(Color.LTGRAY);
        hint.setPadding(0, pad / 2, 0, pad);
        overlay.addView(hint);

        LinearLayout results = new LinearLayout(this);
        results.setOrientation(LinearLayout.VERTICAL);
        ScrollView scroll = new ScrollView(this);
        scroll.addView(results);
        overlay.addView(scroll, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f));

        root.addView(overlay, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        wireHostDiscovery(root, overlay, results, hint);
        buildManualEntrySection(root, overlay, pad);
    }

    /** Starts mDNS discovery and wires found hosts into tappable entries in
     * {@code results}. Split out of {@link #showDiscoveryOverlay} so that
     * method reads as "build the screen", not "build the screen and also
     * run a discovery listener inline". */
    private void wireHostDiscovery(FrameLayout root, LinearLayout overlay, LinearLayout results, TextView hint) {
        java.util.Set<String> shown = new java.util.LinkedHashSet<>();
        hostDiscovery = new HostDiscovery(this, new HostDiscovery.Listener() {
            @Override public void onHostFound(String name, String foundHost, int foundPort, String foundPubkey) {
                runOnUiThread(() -> {
                    if (shown.contains(name)) return;
                    Button entry = new Button(MainActivity.this);
                    entry.setText(name + "\n" + foundHost + ":" + foundPort);
                    entry.setOnClickListener(v ->
                            promptForTokenAndConnect(root, overlay, foundHost, foundPort, foundPubkey));
                    results.addView(entry);
                    shown.add(name);
                });
            }
            @Override public void onHostLost(String name) {
                // Left in the list -- it may just be a transient mDNS blip,
                // and removing a button out from under a mid-tap user is
                // worse than leaving a stale entry that fails to connect.
            }
            @Override public void onDiscoveryFailed(int errorCode) {
                runOnUiThread(() -> hint.setText("Discovery failed (error " + errorCode + "). "
                        + "Enter connection details manually below."));
            }
        });
        hostDiscovery.start();
    }

    /** Manual fallback is always available -- discovery can fail for
     * reasons outside our control (AP client isolation, a network that
     * blocks multicast, etc; see the plan's §9 edge cases). */
    private void buildManualEntrySection(FrameLayout root, LinearLayout overlay, int pad) {
        TextView manualLabel = new TextView(this);
        manualLabel.setText("Or enter manually:");
        manualLabel.setTextColor(Color.LTGRAY);
        manualLabel.setPadding(0, pad, 0, 0);
        overlay.addView(manualLabel);

        EditText manualHost = new EditText(this);
        manualHost.setHint("host (e.g. 192.168.1.42)");
        overlay.addView(manualHost);
        EditText manualPort = new EditText(this);
        manualPort.setHint("port (e.g. 9999)");
        manualPort.setInputType(InputType.TYPE_CLASS_NUMBER);
        overlay.addView(manualPort);
        EditText manualPubkey = new EditText(this);
        manualPubkey.setHint("pubkey (64 hex chars, from the host's terminal output)");
        overlay.addView(manualPubkey);
        Button manualGo = new Button(this);
        manualGo.setText("Next");
        manualGo.setOnClickListener(v -> {
            String h = manualHost.getText().toString().trim();
            String pStr = manualPort.getText().toString().trim();
            String pk = manualPubkey.getText().toString().trim();
            if (h.isEmpty() || pStr.isEmpty() || pk.isEmpty()) return;
            try {
                promptForTokenAndConnect(root, overlay, h, Integer.parseInt(pStr), pk);
            } catch (NumberFormatException ignored) {}
        });
        overlay.addView(manualGo);
    }

    private void promptForTokenAndConnect(
            FrameLayout root, LinearLayout overlay, String foundHost, int foundPort, String foundPubkey) {
        if (hostDiscovery != null) hostDiscovery.stop();

        LinearLayout tokenRow = new LinearLayout(this);
        tokenRow.setOrientation(LinearLayout.VERTICAL);
        int pad = (int) (16 * getResources().getDisplayMetrics().density);
        tokenRow.setPadding(pad, pad, pad, pad);
        tokenRow.setBackgroundColor(Color.BLACK);

        TextView label = new TextView(this);
        label.setText("Connecting to " + foundHost + ":" + foundPort + "\n\nPairing token "
                + "(shown on the host's terminal/journal when palmtopd starts):");
        label.setTextColor(Color.WHITE);
        tokenRow.addView(label);

        EditText tokenInput = new EditText(this);
        tokenInput.setHint("token");
        tokenRow.addView(tokenInput);

        // Pre-filled when mDNS supplied it (the normal case); left editable
        // as a fallback in case an older host doesn't advertise it yet, or
        // discovery didn't resolve TXT records for some reason.
        TextView pubkeyLabel = new TextView(this);
        pubkeyLabel.setText("Host public key (for encryption -- auto-filled when discovered):");
        pubkeyLabel.setTextColor(Color.WHITE);
        tokenRow.addView(pubkeyLabel);
        EditText pubkeyInput = new EditText(this);
        pubkeyInput.setHint("pubkey (64 hex chars)");
        pubkeyInput.setText(foundPubkey == null ? "" : foundPubkey);
        tokenRow.addView(pubkeyInput);

        Button connectBtn = new Button(this);
        connectBtn.setText("Connect");
        tokenRow.addView(connectBtn);

        root.addView(tokenRow, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        connectBtn.setOnClickListener(v -> {
            String enteredToken = tokenInput.getText().toString().trim();
            String enteredPubkey = pubkeyInput.getText().toString().trim();
            if (enteredPubkey.isEmpty()) {
                pubkeyLabel.setTextColor(Color.RED);
                pubkeyLabel.setText("Host public key is required -- check the host's terminal output.");
                return;
            }
            completeConnectionSetup(root, java.util.Arrays.asList(tokenRow, overlay),
                    foundHost, foundPort, enteredToken, enteredPubkey);
        });
    }

    /**
     * Shared tail end of every pairing path (manual entry, mDNS-assisted
     * entry, and QR scan): persist the connection details, tear down
     * whichever setup views are still showing, and kick off the connection.
     */
    private void completeConnectionSetup(
            FrameLayout root, List<View> viewsToRemove, String h, int p, String t, String pk) {
        host = h;
        port = p;
        token = t;
        pubkey = pk;
        new ConnectionState(host, port, token, pubkey, currentMode).save(this);

        for (View v : viewsToRemove) {
            root.removeView(v);
        }
        setControlsVisible(true);
        wireSurfaceCallbacks();
        // The surface almost certainly already exists (it's been in the view
        // hierarchy since onCreate, just obscured) -- addCallback alone won't
        // re-fire surfaceCreated for an already-created surface, so kick off
        // the connection directly too.
        surfaceHolder = surfaceView.getHolder();
        startConnection();
    }

    /**
     * (Re)starts the connection from scratch: tears down any previous socket
     * and codec, bumps {@link #generation} so the old network/writer threads
     * (if still winding down) recognise themselves as stale and stop touching
     * shared state, then spins up a fresh network thread.
     */
    private void startConnection() {
        if (surfaceHolder == null || host == null) return;
        teardown();
        int myGeneration = ++generation;
        statusView.setTextColor(Color.GREEN);
        statusView.setText("connecting to " + host + ":" + port + " ...");
        new Thread(() -> runNetwork(surfaceHolder, myGeneration), "palmtop-net").start();
    }

    private void teardown() {
        connected = false;
        decodedFrames = 0;
        droppedFrames = 0;
        noise = null;
        outbox.clear();
        try { if (socket != null) socket.close(); } catch (IOException ignored) {}
        socket = null;
        releaseCodec();
    }

    /**
     * Tears the decoder down so a new one can be configured for a different
     * resolution.
     *
     * Resetting availableInputs and the inFlight permit is not optional
     * housekeeping: a stale permit count would silently throttle the rebuilt
     * decoder to fewer frames in flight than intended, or leave a permit
     * outstanding that nothing will ever release, stalling playback with no
     * error anywhere.
     */
    private void releaseCodec() {
        try { if (codec != null) { codec.stop(); codec.release(); } } catch (Exception ignored) {}
        codec = null;
        if (codecThread != null) codecThread.quitSafely();
        codecThread = null;
        availableInputs.clear();
        inFlight.drainPermits();
        inFlight.release();
        queuedAtUs = 0;
    }

    // ------------------------------------------------------------ quality modes

    /**
     * Presets trade picture quality and power against sync. What each one
     * actually does lives on the host (palmtopd's modes.rs) and arrives in
     * VideoConfig -- the client only names them and asks (see {@link Modes}).
     * Keeping the numbers in one place means the two ends cannot quietly
     * disagree about what "Sync mode" means.
     */
    private void showModePicker() {
        String[] items = new String[Modes.NAMES.length];
        for (int i = 0; i < Modes.NAMES.length; i++) {
            items[i] = (i == currentMode ? "● " : "○ ") + Modes.NAMES[i];
        }
        new android.app.AlertDialog.Builder(this)
                .setTitle("Quality mode")
                .setItems(items, (dialog, which) -> selectMode(which))
                .show();
    }

    private void selectMode(int mode) {
        if (!Modes.isValid(mode)) return;
        currentMode = mode;
        ConnectionState.saveMode(this, mode);
        updateModeButton();
        if (connected) {
            enqueue(Protocol.setMode(mode));
            // The button and drop budget update again when the host's
            // VideoConfig arrives -- that is the authoritative answer, and it
            // may differ from what was requested if the host declined it.
        }
    }

    private void updateModeButton() {
        if (modeButton != null) modeButton.setText("⚙ " + Modes.nameOf(currentMode));
    }

    // ------------------------------------------------------------ aspect ratio

    /**
     * Purely a local display preference (see {@link AspectMode}) -- unlike
     * quality Mode, this never touches the wire, so it never needs to wait
     * for the host to answer back; the picture re-fits the instant a preset
     * is chosen.
     */
    private void showAspectPicker() {
        String[] items = new String[AspectMode.NAMES.length];
        for (int i = 0; i < AspectMode.NAMES.length; i++) {
            items[i] = (i == currentAspectMode ? "● " : "○ ") + AspectMode.NAMES[i];
        }
        new android.app.AlertDialog.Builder(this)
                .setTitle("Aspect ratio")
                .setItems(items, (dialog, which) -> selectAspectMode(which))
                .show();
    }

    private void selectAspectMode(int mode) {
        if (!AspectMode.isValid(mode)) return;
        currentAspectMode = mode;
        ConnectionState.saveAspectMode(this, mode);
        updateAspectButton();
        resizeSurfaceToFit(knownVideoWidth, knownVideoHeight);
    }

    private void updateAspectButton() {
        if (aspectButton != null) aspectButton.setText("▭ " + AspectMode.nameOf(currentAspectMode));
    }

    // ------------------------------------------------------------ touch input

    /**
     * Direct touch = absolute position, exactly like tapping a touchscreen.
     * ACTION_DOWN both positions the cursor *and* presses the button at that
     * position (rather than disambiguating tap-vs-drag first) -- this is what
     * makes a drag natively fall out for free: press-move-release here is
     * just "hold the button down while moving", the same as physically
     * dragging a mouse. No separate trackpad mode; see the class doc comment
     * for why this replaced it outright rather than existing alongside it.
     *
     * A 3+-finger touch is handled entirely by {@link #pinchZoom} instead --
     * see its class doc comment for why, once a gesture goes there, it stays
     * there until every finger lifts. Only once that returns false (an
     * ordinary sub-3-finger touch) does a tap ever reach the host.
     *
     * Because the aspect-mode crop and the interactive zoom are both
     * expressed as {@link #surfaceView} being rendered larger than (and
     * possibly panned within) its own laid-out bounds -- see
     * {@link VideoFit#computePlacement} and {@link PinchZoomController} --
     * a plain {@code event.getX()/getWidth()} is no longer enough on its
     * own; {@link #mapToVideoFraction} does the same job, accounting for both.
     */
    private boolean onTouch(View v, MotionEvent event) {
        if (pinchZoom.onTouch(event)) return true;
        if (!connected) return true;
        if (surfaceView.getWidth() == 0 || surfaceView.getHeight() == 0) return true;

        float[] frac = mapToVideoFraction(event.getX(), event.getY());
        float nx = frac[0], ny = frac[1];

        switch (event.getActionMasked()) {
            case MotionEvent.ACTION_DOWN:
                enqueue(Protocol.pointerMotionAbsolute(nx, ny));
                enqueue(Protocol.pointerButton(Protocol.BUTTON_LEFT, true));
                return true;
            case MotionEvent.ACTION_MOVE:
                enqueue(Protocol.pointerMotionAbsolute(nx, ny));
                return true;
            case MotionEvent.ACTION_UP:
            case MotionEvent.ACTION_CANCEL:
                enqueue(Protocol.pointerButton(Protocol.BUTTON_LEFT, false));
                return true;
            default:
                return true;
        }
    }

    /**
     * Maps a touch position -- given in {@link #videoClip}'s stable,
     * untransformed coordinate space (see {@link #wireSurfaceCallbacks} for
     * why the listener lives there, not on surfaceView itself) -- to a
     * {@code [0,1]} fraction of the actual video content, exactly what
     * {@code onTouch} always sent even before cropping or zoom existed.
     * {@code surfaceView.getLeft()/getTop()} below are relative to
     * videoClip, its real immediate parent -- matching what this method
     * receives as {@code containerX}/{@code containerY}.
     *
     * Explicit inverse-transform math, not Android's own automatic
     * per-view touch correction: surfaceView's scale is being changed
     * *during* the very gesture that would be reading it back mid-pinch,
     * and doing this deterministically avoids ever depending on exactly
     * when that correction is applied relative to a live-changing transform.
     */
    private float[] mapToVideoFraction(float containerX, float containerY) {
        float scale = surfaceView.getScaleX(); // scaleY is always set identically, see PinchZoomController
        if (scale <= 0f) scale = 1f;
        float w = surfaceView.getWidth();
        float h = surfaceView.getHeight();
        float renderedLeft = surfaceView.getLeft() + w / 2f - scale * w / 2f + surfaceView.getTranslationX();
        float renderedTop = surfaceView.getTop() + h / 2f - scale * h / 2f + surfaceView.getTranslationY();
        float localX = (containerX - renderedLeft) / scale;
        float localY = (containerY - renderedTop) / scale;
        return new float[] { localX / w, localY / h };
    }

    /** Shows the soft keyboard on a button tap. Auto-showing on every tap was
     * tried and reverted -- there's no way to know whether a given tap landed
     * on something text-editable on the remote screen, and in practice it
     * prompted the keyboard far too often to be usable. An explicit button
     * the user presses only when they actually want to type is better. */
    private void showKeyboard() {
        hiddenInput.requestFocus();
        InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
        if (imm != null) imm.showSoftInput(hiddenInput, 0);
    }

    private void sendChar(char c) {
        if (c == '\b') {
            enqueue(Protocol.key(Keycodes.KEY_BACKSPACE, true, 0));
            enqueue(Protocol.key(Keycodes.KEY_BACKSPACE, false, 0));
            return;
        }
        int[] mapping = Keycodes.lookup(c);
        if (mapping == null) {
            Log.w(TAG, "no keycode mapping for '" + c + "' -- dropped (ASCII only for now)");
            return;
        }
        int code = mapping[0];
        int mods = mapping[1] != 0 ? Protocol.MOD_SHIFT : 0;
        enqueue(Protocol.key(code, true, mods));
        enqueue(Protocol.key(code, false, mods));
    }

    private void enqueue(byte[] framed) {
        if (!outbox.offer(framed)) {
            Log.w(TAG, "outbox full, dropping an input event");
        }
    }

    // ------------------------------------------------------------ networking

    /**
     * @param myGeneration snapshot of {@link #generation} at connect time --
     *     lets this thread recognise once {@link #startConnection()} (or
     *     {@link #teardown()}) has superseded it, so it stops touching shared
     *     UI/codec state instead of racing a newer connection attempt.
     */
    private void runNetwork(SurfaceHolder holder, int myGeneration) {
        Socket mySocket = null;
        try {
            mySocket = new Socket();
            socket = mySocket; // published so teardown() can close it from another thread
            mySocket.connect(new InetSocketAddress(host, port), 10000);
            mySocket.setTcpNoDelay(true);
            Log.i(TAG, "connected to " + host + ":" + port);

            // Deliberately NOT a BufferedInputStream: it would read ahead and
            // buffer bytes belonging to whichever phase (handshake vs. Noise
            // transport frames) comes next past whatever it happened to grab,
            // silently losing them from that phase's point of view.
            // DataInputStream reads exactly what's asked for per call with no
            // internal read-ahead, so the same instance is safe to use across
            // both the handshake and everything after it.
            OutputStream rawOut = mySocket.getOutputStream();
            DataInputStream rawIn = new DataInputStream(mySocket.getInputStream());

            performHandshake(rawIn, rawOut);

            Protocol.Received cfg = readInitialVideoConfig(rawIn);
            applyInitialVideoConfig(cfg, myGeneration);

            configureCodec(holder, cfg.width, cfg.height);
            connected = true;

            Thread writer = new Thread(() -> runWriter(rawOut, myGeneration), "palmtop-writer");
            writer.start();

            while (generation == myGeneration) {
                Protocol.Received msg = recvEncrypted(rawIn);
                if (msg == null) {
                    Log.i(TAG, "host closed the connection");
                    break;
                }
                if (msg.tag == Protocol.TAG_VIDEO_FRAME) {
                    handleVideoFrame(msg, rawIn, myGeneration);
                } else if (msg.tag == Protocol.TAG_PONG) {
                    latency.onPong(msg.tClientUs, msg.tHostRecvUs, msg.tHostSendUs, nowUs());
                } else if (msg.tag == Protocol.TAG_VIDEO_CONFIG) {
                    handleVideoConfigChange(msg, holder);
                }
            }
            writer.interrupt();
        } catch (Exception e) {
            Log.e(TAG, "network thread failed", e);
            if (generation == myGeneration) {
                runOnUiThread(() -> {
                    statusView.setTextColor(Color.RED);
                    statusView.setText("ERROR: " + e + "\n\ntap ⟳ Reconnect to retry");
                });
            }
        } finally {
            if (generation == myGeneration) connected = false;
            try { if (mySocket != null) mySocket.close(); } catch (IOException ignored) {}
        }
    }

    /** Noise handshake, then the palmtop-proto Hello/HelloAck pairing check.
     * Sets {@link #noise}; throws on any rejection or unexpected message. */
    private void performHandshake(DataInputStream rawIn, OutputStream rawOut) throws Exception {
        byte[] hostPubKey = hexDecode(pubkey);
        noise = NoiseTransport.handshakeInitiator(rawIn, rawOut, hostPubKey);
        Log.i(TAG, "noise handshake ok");

        sendEncrypted(rawOut, Protocol.hello(token));
        Protocol.Received ack = recvEncrypted(rawIn);
        if (ack == null || ack.tag != Protocol.TAG_HELLO_ACK || !ack.ok) {
            String reason = ack != null ? ack.reason : "connection closed during handshake";
            throw new IOException("handshake rejected: " + reason);
        }
        Log.i(TAG, "handshake ok");
    }

    /** The first message after a successful handshake is always VideoConfig
     * -- anything else means a protocol mismatch or a broken connection. */
    private Protocol.Received readInitialVideoConfig(DataInputStream rawIn) throws Exception {
        Protocol.Received cfg = recvEncrypted(rawIn);
        if (cfg == null || cfg.tag != Protocol.TAG_VIDEO_CONFIG) {
            throw new IOException("expected VideoConfig, got " + (cfg == null ? "EOF" : cfg.tag));
        }
        return cfg;
    }

    /** Records the stream's starting format, re-asserts the user's chosen
     * mode if it differs from the host's default, and fits the video
     * surface to it. */
    private void applyInitialVideoConfig(Protocol.Received cfg, int myGeneration) {
        videoConfig = cfg;
        // The host opens in its own default. If the user picked something
        // else previously, re-assert it now rather than silently reverting
        // them on every reconnect.
        if (currentMode != cfg.mode) {
            enqueue(Protocol.setMode(currentMode));
        } else {
            currentMode = cfg.mode;
        }
        Log.i(TAG, "video config: " + cfg.codec + " " + cfg.width + "x" + cfg.height + "@" + cfg.fps
                + " mode=" + cfg.mode + " dropBudget=" + cfg.dropBudgetMs + "ms");
        dropBudgetUs = cfg.dropBudgetMs * 1000L;
        if (generation == myGeneration) {
            runOnUiThread(() -> {
                statusView.setText("connected " + host + ":" + port + "\n"
                        + cfg.width + "x" + cfg.height + "@" + cfg.fps + "fps");
                resizeSurfaceToFit(cfg.width, cfg.height);
            });
        }
    }

    /**
     * One incoming video frame: decide whether decoding it is worth doing,
     * then report stats if this was the periodic Nth frame.
     *
     * The host already drops stale *unsent* frames (see palmtopd's
     * LatestEncoded), but once a frame is written to the socket, TCP delivers
     * it -- and everything queued behind it -- in order no matter what.
     * Nothing on this side ever skipped ahead, so if decode was ever even
     * slightly slower than arrival on average (sustained high-motion content,
     * e.g. video playback), the gap compounded without bound instead of
     * self-correcting: the observed symptom was the picture falling further
     * and further behind over time, never catching up.
     *
     * Drop only when a newer frame is ALREADY buffered locally. That is the
     * condition under which dropping is free: something strictly better is
     * about to supersede this on screen anyway, so skipping it costs nothing
     * and stops the gap compounding.
     *
     * An age-against-a-budget rule was tried here and measured *worse*, which
     * is worth recording because it sounds more principled. Steady-state e2e
     * on this link is ~117ms while the balanced budget is 80ms, so a pure age
     * test discarded ~45% of frames even when the pipeline was keeping up and
     * nothing newer was waiting. Those drops buy no latency back -- the next
     * frame is equally late -- they just throw away frames already paid for
     * in bandwidth and decryption, and lower the framerate. The useful
     * question is not "is this frame old?" but "is there a better one right
     * behind it?".
     *
     * Frame age is still measured (it drives the HUD and the benchmark), it
     * just is not what decides this.
     *
     * Keyframes are never skipped, no matter how much is buffered behind
     * them: they carry SPS/PPS and are the only frames the decoder can
     * (re)start from. Skipping one leaves the decoder trying to decode
     * P-frames with no reference context -- it accepts them without an
     * exception but never produces output, which starves the inFlight permit
     * forever since nothing completes. (Found by hitting exactly this: every
     * frame silently "decoder saturated" from the very first one, because an
     * early version of this skip check dropped the opening keyframe along
     * with the backlog behind it.)
     */
    private void handleVideoFrame(Protocol.Received msg, DataInputStream rawIn, int myGeneration) throws Exception {
        boolean supersededByNewerFrame = !msg.keyframe && rawIn.available() > 0;
        if (supersededByNewerFrame) {
            droppedFrames++;
            latency.recordDrop();
        } else {
            decodedFrames++;
            feedDecoder(msg.data, msg.captureUs);
        }
        reportStatsIfDue(myGeneration);
    }

    /** Logs a machine-readable stats line (parsed by
     * scripts/measure-latency.sh) and refreshes the on-screen status/HUD,
     * once every 30 frames. */
    private void reportStatsIfDue(int myGeneration) {
        if (generation != myGeneration || (decodedFrames + droppedFrames) % 30 != 0) return;
        long d = decodedFrames, sk = droppedFrames;
        Protocol.Received vc = videoConfig;
        LatencyTracker.Stats stats = latency.snapshot();
        Log.i(TAG, String.format(java.util.Locale.US,
                "stats mode=%s e2e_p50_us=%d e2e_p95_us=%d rtt_p50_us=%d "
                        + "decode_p50_us=%d drop_pct=%.2f w=%d h=%d fps=%d valid=%b",
                Modes.nameOf(currentMode), stats.e2eP50, stats.e2eP95, stats.rttP50,
                stats.decodeP50, stats.dropPercent, vc.width, vc.height, vc.fps,
                stats.valid));
        runOnUiThread(() -> {
            statusView.setText(host + ":" + port + "  "
                    + vc.width + "x" + vc.height + "@" + vc.fps + "fps\n"
                    + "decoded " + d + "  dropped-stale " + sk);
            hud.update(stats, Modes.nameOf(currentMode), vc.width, vc.height, vc.fps);
        });
    }

    /** A mode change. TCP is ordered, so *every* frame after this message is
     * in the announced format -- there is no ambiguity about which frames
     * belong to which config, and no need to buffer or guess. Rebuilds the
     * decoder (if the resolution actually changed) and re-fits the video
     * surface to it. */
    private void handleVideoConfigChange(Protocol.Received msg, SurfaceHolder holder) throws IOException {
        Protocol.Received previous = videoConfig;
        videoConfig = msg;
        currentMode = msg.mode;
        dropBudgetUs = msg.dropBudgetMs * 1000L;
        if (previous == null || msg.width != previous.width || msg.height != previous.height) {
            Log.i(TAG, "stream format changed to " + msg.width + "x" + msg.height
                    + " -- rebuilding decoder");
            releaseCodec();
            configureCodec(holder, msg.width, msg.height);
        }
        runOnUiThread(() -> {
            updateModeButton();
            resizeSurfaceToFit(msg.width, msg.height);
        });
    }

    private void runWriter(OutputStream out, int myGeneration) {
        long nextPingAt = 0, pingsSent = 0, nonce = 0;
        try {
            while (generation == myGeneration) {
                if (nowUs() >= nextPingAt) {
                    // Sent directly rather than through the outbox: the
                    // timestamp has to be taken immediately before the
                    // write, or our own queuing delay lands inside the
                    // measured network RTT. The host stamps its side in
                    // its writer for exactly the same reason.
                    sendEncrypted(out, Protocol.ping(++nonce, nowUs()));
                    pingsSent++;
                    nextPingAt = nowUs() + (pingsSent < PING_BURST
                            ? PING_BURST_INTERVAL_US : PING_STEADY_INTERVAL_US);
                }
                // Short poll so the ping schedule stays accurate; input
                // events still go out the instant they are enqueued.
                byte[] msg = outbox.poll(100, TimeUnit.MILLISECONDS);
                if (msg == null) continue;
                sendEncrypted(out, msg);
            }
        } catch (Exception e) {
            Log.i(TAG, "writer thread stopping: " + e);
        }
    }

    // ------------------------------------------------------------ noise transport

    /** Encrypts an already-framed plaintext palmtop-proto message and writes
     * it as one or more Noise wire frames. */
    private void sendEncrypted(OutputStream out, byte[] framedPlaintext) throws Exception {
        for (byte[] frame : noise.chunkAndEncrypt(framedPlaintext)) {
            out.write(frame);
        }
        out.flush();
    }

    /** Blocks for one full decrypted message (possibly reassembled from
     * several wire chunks). Returns null on a clean EOF at a chunk boundary. */
    private Protocol.Received recvEncrypted(DataInputStream rawIn) throws Exception {
        NoiseTransport.Reassembler reassembler = new NoiseTransport.Reassembler();
        byte[] complete;
        while (true) {
            byte[] ciphertext = NoiseTransport.readOneFrame(rawIn);
            if (ciphertext == null) return null;
            byte[] plaintext = noise.decryptChunk(ciphertext);
            complete = reassembler.push(plaintext);
            if (complete != null) break;
        }
        return Protocol.readMessage(new DataInputStream(new ByteArrayInputStream(complete)));
    }

    private static byte[] hexDecode(String s) {
        int len = s.length();
        byte[] out = new byte[len / 2];
        for (int i = 0; i < out.length; i++) {
            out[i] = (byte) Integer.parseInt(s.substring(i * 2, i * 2 + 2), 16);
        }
        return out;
    }

    // ------------------------------------------------------------ decode

    private String pickDecoder() {
        MediaCodecList list = new MediaCodecList(MediaCodecList.REGULAR_CODECS);
        String fallback = null;
        for (MediaCodecInfo info : list.getCodecInfos()) {
            if (info.isEncoder()) continue;
            for (String type : info.getSupportedTypes()) {
                if (!type.equalsIgnoreCase(MIME)) continue;
                String name = info.getName();
                if (name.contains("low_latency")) return name;
                if (fallback == null && !name.startsWith("OMX.google") && !name.startsWith("c2.android")) {
                    fallback = name;
                }
            }
        }
        return fallback;
    }

    private void configureCodec(SurfaceHolder holder, int width, int height) throws IOException {
        String decoderName = pickDecoder();
        Log.i(TAG, "using decoder: " + decoderName);

        MediaFormat fmt = MediaFormat.createVideoFormat(MIME, width, height);
        fmt.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
        fmt.setInteger(MediaFormat.KEY_PRIORITY, 0);

        codecThread = new HandlerThread("palmtop-codec");
        codecThread.start();
        Handler codecHandler = new Handler(codecThread.getLooper());

        codec = MediaCodec.createByCodecName(decoderName);
        codec.setCallback(createDecoderCallback(), codecHandler);
        codec.configure(fmt, holder.getSurface(), null, 0);
        codec.start();
    }

    private MediaCodec.Callback createDecoderCallback() {
        return new MediaCodec.Callback() {
            @Override
            public void onInputBufferAvailable(MediaCodec mc, int index) {
                // Must return immediately -- MediaCodec runs every callback on
                // this one handler thread, so blocking here would stall
                // onOutputBufferAvailable behind it (this exact bug cost the
                // Phase 0 decode spike a spurious 133ms measurement).
                availableInputs.offer(index);
            }
            @Override
            public void onOutputBufferAvailable(MediaCodec mc, int index, MediaCodec.BufferInfo info) {
                long outUs = nowUs();
                long decodeUs = queuedAtUs == 0 ? 0 : outUs - queuedAtUs;
                inFlight.release();
                // presentationTimeUs is the host capture time we queued with.
                // Converting it here (rather than on the way in) keeps the
                // codec's timestamps monotonic -- see feedDecoder.
                if (info.presentationTimeUs != 0 && latency.hasOffset()) {
                    long captureClientUs = info.presentationTimeUs - latency.offsetUs();
                    latency.recordFrame(outUs - captureClientUs, decodeUs);
                }
                mc.releaseOutputBuffer(index, true);
            }
            @Override public void onError(MediaCodec mc, MediaCodec.CodecException e) {
                Log.e(TAG, "codec error", e);
            }
            @Override public void onOutputFormatChanged(MediaCodec mc, MediaFormat f) {
                Log.i(TAG, "output format: " + f);
            }
        };
    }

    /**
     * @param captureUs the host's capture timestamp, passed as MediaCodec's
     *     presentation time so the codec hands it straight back at output --
     *     no side map needed. Deliberately left on the *host's* monotonic
     *     clock: converting to client time here would break MediaCodec's
     *     requirement that presentation timestamps increase monotonically,
     *     because the clock offset is re-estimated as probes arrive and can
     *     step backwards.
     */
    private void feedDecoder(byte[] au, long captureUs) {
        try {
            if (!inFlight.tryAcquire(500, TimeUnit.MILLISECONDS)) {
                Log.w(TAG, "decoder saturated -- dropping frame");
                return;
            }
            Integer index = availableInputs.poll(500, TimeUnit.MILLISECONDS);
            if (index == null) {
                inFlight.release();
                return;
            }
            ByteBuffer buf = codec.getInputBuffer(index);
            buf.clear();
            buf.put(au);
            queuedAtUs = nowUs();
            codec.queueInputBuffer(index, 0, au.length, captureUs, 0);
        } catch (Exception e) {
            Log.e(TAG, "feedDecoder", e);
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQUEST_SCAN_QR || resultCode != RESULT_OK || data == null) return;
        String h = data.getStringExtra("host");
        int p = data.getIntExtra("port", 0);
        String t = data.getStringExtra("token");
        String pk = data.getStringExtra("pubkey");
        if (h == null || p == 0 || t == null || pk == null) return;
        if (hostDiscovery != null) hostDiscovery.stop();
        completeConnectionSetup(discoveryRoot, java.util.Collections.singletonList(discoveryOverlayView), h, p, t, pk);
    }

    @Override
    protected void onDestroy() {
        generation++; // orphans any in-flight network/writer threads
        teardown();
        super.onDestroy();
    }
}
