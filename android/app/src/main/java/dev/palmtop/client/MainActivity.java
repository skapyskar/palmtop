package dev.palmtop.client;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.Intent;
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
import android.text.SpannableStringBuilder;
import android.text.Spanned;
import android.text.style.ForegroundColorSpan;
import android.view.View;
import android.view.WindowManager;
import android.view.inputmethod.InputMethodManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.SeekBar;
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
    // Cross-thread: the network thread reads this for every configureCodec
    // call rather than trusting a value captured once at connect time, so a
    // surface recreated mid-session (see wireSurfaceCallbacks) is never
    // configured against a stale, already-destroyed holder.
    private volatile SurfaceHolder surfaceHolder;
    /** The video's actual home -- see {@link #buildVideoContainer()} and
     *  {@link #resizeSurfaceToFit}. */
    private FrameLayout videoContainer;
    /** The real crop boundary, sized to exactly the visible rect and
     *  centered within {@link #videoContainer} -- see
     *  {@link #buildVideoContainer()}'s doc comment for why this exists as
     *  a separate view from videoContainer itself. */
    private FrameLayout videoClip;
    /** The activity's root view, kept so overlays (Devices, discovery)
     *  can be attached and removed without re-deriving it from the view
     *  tree each time. */
    private FrameLayout rootLayout;
    private Button logButton;
    private View logOverlayView;
    private View settingsOverlayView;
    private Button settingsButton;
    /** Empty container in the left column that {@link #wireJoystick()}
     *  fills. Kept as a field so the column's construction stays one
     *  readable method rather than growing a second responsibility. */
    private LinearLayout joystickSlot;
    /** Last status line and its colour. The status view lives in the
     *  settings sheet now, which is detached most of the time -- so the
     *  text has to survive independently of the view that shows it, or
     *  opening Settings would show a blank line until something next
     *  happened to update it. */
    private String statusText = "";
    private int statusColor = Ui.TEXT_MUTED;
    private TextView statusView;
    private EditText hiddenInput;
    private Button kbToggle;
    private Button reconnectButton;
    private Button modeButton;
    private Button aspectButton;
    private Button hudToggle;
    private Button devicesButton;
    private HudView hud;
    /** Turns 3+-finger touches into a local zoom/pan on {@link #surfaceView}
     *  -- see its class doc comment. Constructed once, alongside surfaceView,
     *  in {@link #buildVideoContainer()}. */
    private PinchZoomController pinchZoom;
    /** Drives the cursor from the joystick -- see {@link #wireJoystick()}. */
    private CursorDriver cursorDriver;
    /** Pointer buttons this client currently believes are pressed on the
     *  host, indexed by {@code Protocol.BUTTON_*}. Tracked so a connection
     *  that drops mid-drag cannot leave the laptop with a stuck mouse button
     *  -- the same class of bug as the plan's "modifier stuck" edge case. */
    private final boolean[] buttonHeld = new boolean[3];
    /** Latched Ctrl/Alt/Shift/Super -- see {@link ModifierLatch}. */
    private final ModifierLatch modifiers = new ModifierLatch();
    private final java.util.Map<Integer, Button> modifierButtons = new java.util.HashMap<>();
    /** Joystick speed at full deflection, host px/s. Persisted. */
    private float sensitivity = CursorDriver.DEFAULT_SPEED_PX_S;
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

    /** This device's capabilities, sent to the host at handshake so it can
     *  size the stream correctly. Detected once and reused: these are
     *  fixed hardware facts, and re-querying the codec list on every
     *  reconnect would cost time in exactly the latency-sensitive path
     *  this project spends its effort protecting. */
    private volatile DeviceProfile deviceProfile;

    private static long nowUs() { return System.nanoTime() / 1000L; }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);

        // Measured once here rather than per-connection: fixed hardware
        // facts, and the codec-capability query is not free.
        deviceProfile = DeviceProfile.detect(this, MIME);

        // ConnectionState prefers the launching Intent's extras, falling back
        // to whatever was last persisted -- see its class doc comment for why
        // (reopening via the launcher icon sends a bare ACTION_MAIN Intent
        // with no extras).
        // Quality mode and aspect ratio are app-wide settings; which laptop
        // to talk to is a separate concern owned by DeviceStore.
        currentMode = ConnectionState.resolveMode(this, getIntent().getIntExtra("mode", -1));
        currentAspectMode = ConnectionState.loadAspectMode(this);
        sensitivity = ConnectionState.loadSensitivity(this);

        // Three ways in, in priority order:
        //  1. credentials pushed by the laptop's USB pairing step (an Intent
        //     with extras) -- always authoritative, and saved on arrival
        //  2. the most recently used saved device, for an ordinary relaunch
        //  3. nothing saved yet -> the Devices screen, which offers pairing
        PairedDevice launched = deviceFromIntent();
        if (launched != null) {
            DeviceStore.upsert(this, launched.withLastConnectedNow());
            applyDevice(launched);
        } else {
            applyDevice(DeviceStore.mostRecent(this));
        }

        FrameLayout root = buildUi();
        rootLayout = root;
        setContentView(root);

        if (host == null || host.isEmpty() || port == 0 || pubkey == null || pubkey.isEmpty()) {
            setControlsVisible(false);
            showDeviceListOverlay(root);
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
        // A small breathing space above everything. Without the title bar
        // (removed in v0.3.0) the video sat flush against the phone's top
        // edge, which made the laptop's own top-corner UI -- window close
        // buttons, menu bars -- awkward to hit and easy to lose to the
        // system's edge gestures.
        //
        // Applied to this parent rather than to videoContainer deliberately:
        // parent padding reduces the child's allocated size, so
        // videoContainer.getHeight() already reflects the inset and
        // resizeSurfaceToFit re-fits correctly with no change to its logic.
        // Padding videoContainer itself would be silently ignored -- it
        // measures getWidth()/getHeight() *including* its own padding and
        // centers videoClip within the full bounds. Insetting here also
        // fixes the same reachability problem for the column's top button.
        //
        // Costs the video 12dp of height; its aspect ratio is untouched
        // (VideoFit letterboxes as always) and its width is unchanged.
        mainLayout.setPadding(0, Ui.md(this), 0, 0);
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

        // After buildLeftBar(), which creates the slot this fills.
        wireJoystick();

        buildHiddenInput(root);
        return root;
    }

    /**
     * The always-visible controls: settings, keyboard, and the cursor
     * joystick.
     *
     * <p>Everything that is set once and then forgotten -- connection status,
     * reconnect, devices, quality mode, aspect ratio, the log and the stats
     * toggle -- moved behind {@link #showSettings()}. They were permanently
     * occupying the column that the controls used *during* a session should
     * own, which is what made room for the joystick without touching the
     * video.
     *
     * <p>The column keeps its existing fixed width, so the video's width and
     * shape are exactly what they were before this change. That was the hard
     * constraint on this whole redesign.
     */
    private LinearLayout buildLeftBar() {
        LinearLayout bar = new LinearLayout(this);
        bar.setOrientation(LinearLayout.VERTICAL);
        bar.setBackgroundColor(Ui.PANEL);
        bar.setPadding(Ui.md(this), Ui.md(this), Ui.md(this), Ui.md(this));

        LinearLayout topRow = new LinearLayout(this);
        topRow.setOrientation(LinearLayout.HORIZONTAL);

        settingsButton = Ui.iconButton(this, "⚙");
        settingsButton.setOnClickListener(v -> showSettings());
        topRow.addView(settingsButton, iconSlot(0));

        kbToggle = Ui.iconButton(this, "⌨");
        kbToggle.setOnClickListener(v -> showKeyboard());
        topRow.addView(kbToggle, iconSlot(Ui.dp(this, 6)));

        bar.addView(topRow, Ui.stacked(this, 10));

        // Filled by wireJoystick(). Empty here so this method stays "build
        // the column" rather than also "construct and wire an input device".
        joystickSlot = new LinearLayout(this);
        joystickSlot.setOrientation(LinearLayout.VERTICAL);
        bar.addView(joystickSlot, Ui.stacked(this, 10));

        // Below the controls, not above them: the HUD is diagnostic, appears
        // only when toggled on, and would otherwise shove every button down
        // the column the moment it did. It stays in the column rather than
        // moving into the settings sheet because it is meant to be read
        // *while* using the session -- only its toggle moved.
        hud = new HudView(this);
        bar.addView(hud, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));

        return bar;
    }

    /**
     * Builds the joystick and its click buttons into the slot the column left
     * for them.
     *
     * <p>Separate from {@link #buildLeftBar()} because it is doing something
     * different in kind: that method arranges views, this one assembles an
     * input device out of three parts and connects it to the wire.
     *
     * <p>Nothing here needed a protocol change. {@code PointerMotionRelative}
     * and {@code PointerButton} have both been implemented on the host since
     * before this feature existed, so the joystick cannot desync the two ends
     * and this ships as an APK-only change.
     */
    private void wireJoystick() {
        cursorDriver = new CursorDriver((dx, dy) ->
                enqueue(Protocol.pointerMotionRelative(dx, dy)));
        cursorDriver.setMaxSpeed(sensitivity);

        JoystickView stick = new JoystickView(this);
        stick.setListener(cursorDriver::setVector);
        int size = Ui.dp(this, JOYSTICK_SIZE_DP);
        LinearLayout.LayoutParams stickLp = new LinearLayout.LayoutParams(size, size);
        stickLp.gravity = Gravity.CENTER_HORIZONTAL;
        stickLp.bottomMargin = Ui.sm(this);
        joystickSlot.addView(stick, stickLp);

        LinearLayout clicks = new LinearLayout(this);
        clicks.setOrientation(LinearLayout.HORIZONTAL);

        Button left = Ui.iconButton(this, "L");
        wireClickButton(left, Protocol.BUTTON_LEFT);
        clicks.addView(left, iconSlot(0));

        Button right = Ui.iconButton(this, "R");
        wireClickButton(right, Protocol.BUTTON_RIGHT);
        clicks.addView(right, iconSlot(Ui.dp(this, 6)));

        joystickSlot.addView(clicks, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));

        buildModifierBar();

        // Esc and Tab: the two keys a desktop needs constantly that no phone
        // IME offers. Placed after the modifier bar deliberately -- latch Alt
        // and tap Tab for a real Alt+Tab, so the two rows read as one group.
        joystickSlot.addView(
                twoUp("Esc", () -> sendKeyTap(Keycodes.KEY_ESC),
                      "Tab", () -> sendKeyTap(Keycodes.KEY_TAB)),
                Ui.stacked(this, 6));
    }

    /**
     * Ctrl / Alt / Shift / Super, as latching buttons.
     *
     * <p>Laid out two per row rather than four across: at this column width
     * four buttons would each be barely a fingertip wide, and a modifier you
     * mis-tap is worse than one that takes a moment to find -- it silently
     * changes what every subsequent keystroke means.
     *
     * <p>See {@link ModifierLatch} for why these latch rather than being held,
     * and why each sends a real key press rather than only a modifier bit.
     */
    private void buildModifierBar() {
        LinearLayout row = null;
        for (int i = 0; i < ModifierLatch.ALL.length; i++) {
            if (i % 2 == 0) {
                row = new LinearLayout(this);
                row.setOrientation(LinearLayout.HORIZONTAL);
                joystickSlot.addView(row, Ui.stacked(this, 6));
            }
            int mod = ModifierLatch.ALL[i];
            Button b = Ui.iconButton(this, ModifierLatch.labelFor(mod));
            b.setOnClickListener(v -> toggleModifier(mod));
            modifierButtons.put(mod, b);
            row.addView(b, iconSlot(i % 2 == 0 ? 0 : Ui.dp(this, 6)));
        }
    }

    /**
     * Latches or releases one modifier, sending the real key press or release
     * that goes with it.
     *
     * <p>Sending the key itself -- rather than only remembering a bit to OR
     * into later keystrokes -- is what makes a bare Super tap open the
     * overview and what lets a latched Super affect a joystick drag. See
     * {@link ModifierLatch}'s class comment.
     */
    private void toggleModifier(int mod) {
        boolean nowLatched = modifiers.toggle(mod);
        int keycode = ModifierLatch.keycodeFor(mod);
        if (keycode >= 0) {
            enqueue(Protocol.key(keycode, nowLatched, modifiers.mask()));
        }
        refreshModifierButton(mod);
    }

    /** Latched modifiers read as active rather than merely available -- this
     *  is the one piece of state where forgetting it is on silently changes
     *  what every keystroke does. */
    private void refreshModifierButton(int mod) {
        Button b = modifierButtons.get(mod);
        if (b == null) return;
        boolean on = modifiers.isLatched(mod);
        b.setBackground(Ui.pressable(on ? Ui.ACCENT : Ui.RAISED, Ui.HAIRLINE, 10f, this));
        b.setTextColor(on ? Ui.BASE : Ui.TEXT);
    }

    /**
     * Releases every latched modifier on the host and clears the bar.
     *
     * <p>Called from {@link #teardown()}: a session that ends with Super
     * latched would otherwise leave the laptop believing Super is still
     * physically held, which makes the desktop behave bizarrely with no
     * indication why and nothing on the phone still able to fix it.
     */
    private void releaseLatchedModifiers() {
        for (int mod : ModifierLatch.ALL) {
            if (modifiers.isLatched(mod)) {
                int keycode = ModifierLatch.keycodeFor(mod);
                if (keycode >= 0 && connected) {
                    enqueue(Protocol.key(keycode, false, 0));
                }
            }
        }
        modifiers.clear();
        for (int mod : ModifierLatch.ALL) {
            refreshModifierButton(mod);
        }
    }

    /**
     * Sends press and release as the finger goes down and up, rather than
     * synthesising a click on tap.
     *
     * <p>That distinction is the whole reason dragging works: holding L while
     * working the joystick produces a real press-move-release on the host, so
     * you can resize a window edge or select text -- something absolute
     * tap-to-click cannot express at all.
     */
    @SuppressLint("ClickableViewAccessibility") // press/release is the point; a click callback cannot express it
    private void wireClickButton(Button button, int protoButton) {
        button.setOnTouchListener((v, event) -> {
            switch (event.getActionMasked()) {
                case MotionEvent.ACTION_DOWN:
                    v.setPressed(true);
                    sendButton(protoButton, true);
                    return true;
                case MotionEvent.ACTION_UP:
                case MotionEvent.ACTION_CANCEL:
                    v.setPressed(false);
                    sendButton(protoButton, false);
                    return true;
                default:
                    return false;
            }
        });
    }

    /** Sends a pointer button and remembers it, so {@link #teardown()} can
     *  release anything still held if the session ends mid-press. */
    private void sendButton(int protoButton, boolean pressed) {
        if (protoButton >= 0 && protoButton < buttonHeld.length) {
            buttonHeld[protoButton] = pressed;
        }
        enqueue(Protocol.pointerButton(protoButton, pressed));
    }

    /** Height of every button in the column, and the joystick pad's size.
     *
     *  <p>Both trimmed from their first values (42dp / 128dp) to buy vertical
     *  room for more controls. The column is the only place controls can go
     *  -- widening it would narrow the video, which is not on the table -- so
     *  every new button has to be paid for out of the existing height. 34dp
     *  is still above the ~32dp that reads as comfortably tappable at this
     *  width; going further would start trading correctness for capacity. */
    private static final int BUTTON_HEIGHT_DP = 34;
    private static final int JOYSTICK_SIZE_DP = 96;

    /** Equal-width slot in an icon row. Weighted with a zero base width so
     *  the buttons share the column exactly, whatever glyph each one carries. */
    private LinearLayout.LayoutParams iconSlot(int leftMargin) {
        LinearLayout.LayoutParams lp =
                new LinearLayout.LayoutParams(0, Ui.dp(this, BUTTON_HEIGHT_DP), 1f);
        lp.leftMargin = leftMargin;
        return lp;
    }

    /** A two-up row of buttons in the column -- the shape every control pair
     *  here uses, so the layout stays one consistent grid rather than each
     *  addition inventing its own spacing. */
    private LinearLayout twoUp(String leftLabel, Runnable onLeft,
                               String rightLabel, Runnable onRight) {
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        Button left = Ui.iconButton(this, leftLabel);
        left.setOnClickListener(v -> onLeft.run());
        row.addView(left, iconSlot(0));
        Button right = Ui.iconButton(this, rightLabel);
        right.setOnClickListener(v -> onRight.run());
        row.addView(right, iconSlot(Ui.dp(this, 6)));
        return row;
    }

    /**
     * Sends one complete key press and release, carrying whatever modifiers
     * are currently latched.
     *
     * <p>Carrying the latch is what makes the combinations work: latch Alt,
     * tap Tab, and you get a real Alt+Tab rather than a bare Tab that does
     * nothing useful.
     */
    private void sendKeyTap(int keycode) {
        int mods = modifiers.mask();
        enqueue(Protocol.key(keycode, true, mods));
        enqueue(Protocol.key(keycode, false, mods));
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
        videoContainer.setBackgroundColor(Ui.BASE);
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
        // Only the always-visible column controls need toggling now. The rest
        // live in the settings sheet, which is a separate overlay that is
        // simply not open while the device list covers the screen.
        settingsButton.setVisibility(v);
        kbToggle.setVisibility(v);
        joystickSlot.setVisibility(v);
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
            /**
             * A live network session must survive a surface recreation.
             *
             * <p>Found from a real report of a device that could never see
             * any video: every "screen sharing approved" was followed within
             * ~150ms by the connection dying, forever, in a loop that kept
             * re-asking the laptop for permission. The trigger is
             * {@link #resizeSurfaceToFit}, called the instant the first
             * VideoConfig arrives -- it changes surfaceView's actual pixel
             * size, which on some OEM builds causes Android to destroy and
             * recreate the underlying Surface (a known, vendor-dependent
             * SurfaceView quirk; it did not reproduce on the reporter's own
             * phone, which is exactly the shape of a device-specific bug).
             * The old code treated every surfaceCreated as "start a brand
             * new session", so this destroy/recreate forced a full
             * reconnect and a fresh screen-share consent every time --
             * meaning the picture could never actually arrive, no matter
             * how many times the dialog was approved.
             *
             * <p>The fix is to stop conflating the two lifecycles. A
             * connected session with a known {@link #videoConfig} just needs
             * its decoder rebound to the new surface; the socket, the Noise
             * session and the laptop's permission grant are untouched by
             * any of this and must stay that way. Only when there is no
             * live session yet does a new surface mean "start one".
             */
            @Override public void surfaceCreated(SurfaceHolder holder) {
                surfaceHolder = holder;
                if (connected && videoConfig != null) {
                    try {
                        releaseCodec();
                        configureCodec(holder, videoConfig.width, videoConfig.height);
                        SessionLog.info("app",
                                "video surface was recreated by the system -- decoder rebound, "
                                        + "connection untouched");
                    } catch (IOException e) {
                        SessionLog.error("app", "could not rebind the decoder to the new surface: "
                                + e.getMessage());
                    }
                } else {
                    startConnection();
                }
            }
            @Override public void surfaceChanged(SurfaceHolder h, int f, int w, int ht) {}
            /**
             * Deliberately does not touch the network session -- see
             * surfaceCreated. Only the decoder's binding to this now-invalid
             * Surface needs to go; feedDecoder already degrades to silently
             * dropping frames when no codec is consuming them (its input-buffer
             * queue simply stops being filled), so no other guard is needed
             * while this surface is briefly gone.
             */
            @Override public void surfaceDestroyed(SurfaceHolder h) { releaseCodec(); }
        });
    }

    // ------------------------------------------------------------ paired devices

    /**
     * Credentials handed over by the laptop during USB pairing, which pushes
     * them straight into the app as Intent extras over ADB.
     *
     * That cable is the reason this path exists at all: it is genuinely
     * out-of-band, so the host's public key arrives over a channel an
     * attacker on the network cannot reach. Compare the mDNS discovery path,
     * where the key is broadcast over the LAN and a hostile peer on the same
     * network could in principle answer first.
     *
     * @return the device described by the launching Intent, or null when it
     *     carries no pairing extras (an ordinary relaunch from the launcher).
     */
    private PairedDevice deviceFromIntent() {
        String h = getIntent().getStringExtra("host");
        int p = getIntent().getIntExtra("port", 0);
        String t = getIntent().getStringExtra("token");
        String pk = getIntent().getStringExtra("pubkey");
        String name = getIntent().getStringExtra("name");
        if (h == null || h.isEmpty() || p == 0 || pk == null || pk.isEmpty()) return null;
        return new PairedDevice(
                name == null || name.isEmpty() ? h : name,
                h, p, t == null ? "" : t, pk, System.currentTimeMillis());
    }

    /** Reopens the Devices screen mid-session, to switch laptops without
     *  restarting the app. Tears the live session down first so the old
     *  connection cannot keep decoding into a surface the new one is
     *  about to claim. */
    private void openDeviceList() {
        generation++; // orphans the in-flight network/writer threads
        teardown();
        setControlsVisible(false);
        showDeviceListOverlay(rootLayout);
    }

    /** Points this session at a saved device. Null clears the target, which
     *  is what sends onCreate to the Devices screen. */
    private void applyDevice(PairedDevice device) {
        if (device == null) {
            host = null;
            port = 0;
            token = "";
            pubkey = "";
            return;
        }
        host = device.host;
        port = device.port;
        token = device.token;
        pubkey = device.pubkey;
    }

    /**
     * The Devices screen: every laptop this phone has been paired with, plus
     * the ways to add another.
     *
     * Shown automatically when nothing is saved yet, and reachable from the
     * sidebar at any time to switch machines.
     */
    /**
     * Shows the running session log.
     *
     * <p>Reachable in one tap from the main screen on purpose. Both failures
     * that motivated this ("no share prompt appeared", "connected but the
     * screen stayed black") are states where the app looks idle and the only
     * evidence lives on the laptop, so the evidence has to be brought to
     * whoever is actually looking at the phone.
     */
    private void showSessionLog() {
        if (logOverlayView != null) {
            dismissSessionLog();
            return;
        }
        LinearLayout overlay = Ui.sheet(this);

        overlay.addView(Ui.title(this, "Session log"), Ui.stacked(this, 2));
        TextView subtitle = Ui.body(this,
                "What this phone and the laptop each reported, most recent last.");
        subtitle.setTextColor(Ui.TEXT_FAINT);
        overlay.addView(subtitle, Ui.stacked(this, 12));
        overlay.addView(Ui.hairline(this), new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, Ui.dp(this, 1)));

        TextView body = Ui.mono(this);
        ScrollView scroll = new ScrollView(this);
        scroll.setPadding(0, Ui.md(this), 0, Ui.md(this));
        scroll.setClipToPadding(false);
        scroll.addView(body);
        overlay.addView(scroll, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f));

        Runnable refresh = () -> {
            SpannableStringBuilder sb = new SpannableStringBuilder();
            for (SessionLog.Entry e : SessionLog.snapshot()) {
                int start = sb.length();
                sb.append(SessionLog.stamp(e)).append("  [").append(e.stage).append("] ")
                  .append(e.message).append("\n");
                int color;
                switch (e.level) {
                    case ERROR: color = Ui.ERR; break;
                    case WARN:  color = Ui.WARN; break;
                    case GOOD:  color = Ui.OK; break;
                    default:    color = Ui.TEXT_MUTED;
                }
                sb.setSpan(new ForegroundColorSpan(color), start, sb.length(),
                        Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if (sb.length() == 0) {
                sb.append("Nothing logged yet. Tap ⟳ Reconnect to start a connection.");
            }
            body.setText(sb);
            scroll.post(() -> scroll.fullScroll(View.FOCUS_DOWN));
        };
        refresh.run();
        // Follows the session live: the interesting moments (waiting on the
        // share dialog, a stage failing) happen while this is already open.
        SessionLog.setListener(() -> runOnUiThread(refresh));

        LinearLayout buttons = new LinearLayout(this);
        buttons.setOrientation(LinearLayout.HORIZONTAL);

        Button copy = Ui.quietButton(this, "Copy");
        copy.setOnClickListener(v -> {
            android.content.ClipboardManager cm =
                    (android.content.ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
            if (cm != null) {
                cm.setPrimaryClip(android.content.ClipData.newPlainText(
                        "palmtop session log", SessionLog.asText()));
                android.widget.Toast.makeText(this, "Log copied", android.widget.Toast.LENGTH_SHORT)
                        .show();
            }
        });
        LinearLayout.LayoutParams half =
                new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f);
        buttons.addView(copy, half);

        Button close = Ui.primaryButton(this, "Close");
        close.setOnClickListener(v -> dismissSessionLog());
        LinearLayout.LayoutParams halfRight =
                new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f);
        halfRight.leftMargin = Ui.sm(this);
        buttons.addView(close, halfRight);
        overlay.addView(buttons);

        logOverlayView = overlay;
        rootLayout.addView(overlay, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
    }

    private void dismissSessionLog() {
        SessionLog.setListener(null);
        if (logOverlayView != null) {
            rootLayout.removeView(logOverlayView);
            logOverlayView = null;
        }
    }

    /**
     * Everything that is configured rather than operated.
     *
     * <p>Built fresh on each open rather than kept around: these controls are
     * touched rarely, so holding an inflated sheet for the whole session to
     * save a few milliseconds on an occasional tap is the wrong trade. It also
     * means the sheet always reflects current state by construction, with no
     * separate refresh path to keep in sync.
     */
    private void showSettings() {
        if (settingsOverlayView != null) {
            dismissSettings();
            return;
        }
        LinearLayout overlay = Ui.sheet(this);

        overlay.addView(Ui.title(this, "Settings"), Ui.stacked(this, 12));
        overlay.addView(Ui.hairline(this), new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, Ui.dp(this, 1)));

        LinearLayout content = new LinearLayout(this);
        content.setOrientation(LinearLayout.VERTICAL);
        ScrollView scroll = new ScrollView(this);
        scroll.setPadding(0, Ui.md(this), 0, Ui.md(this));
        scroll.setClipToPadding(false);
        scroll.addView(content);
        overlay.addView(scroll, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f));

        // The connection line. Recreated here and immediately re-populated
        // from the retained statusText/statusColor -- the view is detached
        // whenever this sheet is closed, so the *text* has to outlive it.
        statusView = Ui.mono(this);
        statusView.setTextColor(statusColor);
        statusView.setText(statusText);
        content.addView(statusView, Ui.stacked(this, 12));

        reconnectButton = Ui.button(this, "⟳  Reconnect");
        reconnectButton.setOnClickListener(v -> {
            dismissSettings();
            startConnection();
        });
        content.addView(reconnectButton, Ui.stacked(this, 6));

        devicesButton = Ui.button(this, "🖥  Devices");
        devicesButton.setOnClickListener(v -> {
            dismissSettings();
            openDeviceList();
        });
        content.addView(devicesButton, Ui.stacked(this, 6));

        modeButton = Ui.button(this, "");
        modeButton.setOnClickListener(v -> showModePicker());
        content.addView(modeButton, Ui.stacked(this, 6));
        updateModeButton();

        aspectButton = Ui.button(this, "");
        aspectButton.setOnClickListener(v -> showAspectPicker());
        content.addView(aspectButton, Ui.stacked(this, 6));
        updateAspectButton();

        content.addView(buildVolumeRow(), Ui.stacked(this, 6));

        content.addView(buildSensitivityRow(), Ui.stacked(this, 6));

        logButton = Ui.button(this, "📋  Session log");
        logButton.setOnClickListener(v -> {
            dismissSettings();
            showSessionLog();
        });
        content.addView(logButton, Ui.stacked(this, 6));

        hudToggle = Ui.button(this, hud.isHudShown() ? "📊  Hide stats" : "📊  Show stats");
        hudToggle.setOnClickListener(v -> {
            hud.setShown(!hud.isHudShown());
            hudToggle.setText(hud.isHudShown() ? "📊  Hide stats" : "📊  Show stats");
        });
        content.addView(hudToggle, Ui.stacked(this, 0));

        Button close = Ui.primaryButton(this, "Close");
        close.setOnClickListener(v -> dismissSettings());
        overlay.addView(close);

        settingsOverlayView = overlay;
        rootLayout.addView(overlay, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
    }

    /**
     * Laptop volume: mute, down, up.
     *
     * <p>These are ordinary key events -- evdev 113/114/115, which the "us"
     * keymap palmtopd uploads carries as XF86AudioMute / LowerVolume /
     * RaiseVolume (confirmed against the compiled keymap, where evdev 115
     * appears as {@code <VOL+>}). So this needed no host change and no new
     * message: the laptop simply receives the same key press its own keyboard
     * would send.
     *
     * <p>The honest caveat, and it is worth stating rather than discovering:
     * pressing the key only changes the volume if the desktop <em>binds</em>
     * it. GNOME and KDE do out of the box; a bare wlroots compositor such as
     * Hyprland or Sway only does if its config says so. If nothing happens,
     * that is the reason, and the fix belongs in the compositor's config
     * rather than here.
     */
    private LinearLayout buildVolumeRow() {
        LinearLayout box = new LinearLayout(this);
        box.setOrientation(LinearLayout.VERTICAL);
        box.setBackground(Ui.rect(Ui.RAISED, 10f, this));
        box.setPadding(Ui.md(this), Ui.md(this), Ui.md(this), Ui.md(this));

        TextView label = Ui.body(this, "🔊  Laptop volume");
        label.setTypeface(Ui.medium());
        box.addView(label, Ui.stacked(this, 8));

        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);

        Button mute = Ui.iconButton(this, "🔇");
        mute.setOnClickListener(v -> sendKeyTap(Keycodes.KEY_MUTE));
        row.addView(mute, iconSlot(0));

        Button down = Ui.iconButton(this, "−");
        down.setOnClickListener(v -> sendKeyTap(Keycodes.KEY_VOLUMEDOWN));
        row.addView(down, iconSlot(Ui.dp(this, 6)));

        Button up = Ui.iconButton(this, "+");
        up.setOnClickListener(v -> sendKeyTap(Keycodes.KEY_VOLUMEUP));
        row.addView(up, iconSlot(Ui.dp(this, 6)));

        box.addView(row);
        return box;
    }

    /**
     * The joystick sensitivity slider.
     *
     * <p>Applied live as it is dragged, not on release: the only way to judge
     * a pointer speed is to feel it, and a control you have to commit to
     * before finding out what it does makes you guess. The value is saved
     * once the finger lifts rather than on every pixel of travel, so a drag
     * across the whole range is one write instead of a hundred.
     */
    private LinearLayout buildSensitivityRow() {
        LinearLayout box = new LinearLayout(this);
        box.setOrientation(LinearLayout.VERTICAL);
        box.setBackground(Ui.rect(Ui.RAISED, 10f, this));
        box.setPadding(Ui.md(this), Ui.md(this), Ui.md(this), Ui.md(this));

        TextView label = Ui.body(this, "");
        label.setTypeface(Ui.medium());
        box.addView(label);

        TextView value = Ui.mono(this);
        value.setTextColor(Ui.TEXT_FAINT);
        box.addView(value, Ui.stacked(this, 4));

        SeekBar bar = new SeekBar(this);
        // The SeekBar's own units are an implementation detail: it runs
        // 0..range and is mapped onto MIN..MAX px/s, so the slider's ends
        // always mean the supported extremes whatever those are retuned to.
        int range = (int) (CursorDriver.MAX_SPEED_PX_S - CursorDriver.MIN_SPEED_PX_S);
        bar.setMax(range);
        bar.setProgress((int) (sensitivity - CursorDriver.MIN_SPEED_PX_S));

        Runnable render = () -> {
            label.setText("🕹  Joystick sensitivity");
            value.setText(String.format(java.util.Locale.US,
                    "%d px/s at full push", (int) sensitivity));
        };
        render.run();

        bar.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar sb, int progress, boolean fromUser) {
                sensitivity = CursorDriver.clampSpeed(CursorDriver.MIN_SPEED_PX_S + progress);
                if (cursorDriver != null) cursorDriver.setMaxSpeed(sensitivity);
                render.run();
            }

            @Override public void onStartTrackingTouch(SeekBar sb) {}

            @Override public void onStopTrackingTouch(SeekBar sb) {
                ConnectionState.saveSensitivity(MainActivity.this, sensitivity);
            }
        });
        box.addView(bar, Ui.stacked(this, 4));

        return box;
    }

    private void dismissSettings() {
        if (settingsOverlayView != null) {
            rootLayout.removeView(settingsOverlayView);
            settingsOverlayView = null;
        }
    }

    /**
     * Records the connection line and shows it if the settings sheet happens
     * to be open.
     *
     * <p>Single funnel for every status update, because {@link #statusView}
     * now lives in a sheet that is detached most of the time. Writing to the
     * view directly would either NPE or silently update a view nobody can
     * see, and the next open would show a stale or blank line.
     */
    private void setStatus(int color, String text) {
        statusColor = color;
        statusText = text;
        if (statusView != null && settingsOverlayView != null) {
            statusView.setTextColor(color);
            statusView.setText(text);
        }
    }

    private void showDeviceListOverlay(FrameLayout root) {
        LinearLayout overlay = new LinearLayout(this);
        overlay.setOrientation(LinearLayout.VERTICAL);
        overlay.setBackgroundColor(Ui.PANEL);
        overlay.setPadding(Ui.xl(this), Ui.lg(this), Ui.xl(this), Ui.lg(this));
        overlay.setClickable(true);
        discoveryRoot = root;
        discoveryOverlayView = overlay;

        List<PairedDevice> devices = DeviceStore.load(this);

        overlay.addView(Ui.title(this, "Devices"), Ui.stacked(this, 2));

        TextView hint = Ui.body(this, devices.isEmpty()
                ? "No laptops paired yet. Add one over USB, or scan the QR code the laptop "
                  + "prints when you run its install script."
                : "Tap a laptop to connect. Long-press to forget it.");
        overlay.addView(hint, Ui.stacked(this, 12));
        overlay.addView(Ui.hairline(this), new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, Ui.dp(this, 1)));

        LinearLayout list = new LinearLayout(this);
        list.setOrientation(LinearLayout.VERTICAL);
        ScrollView scroll = new ScrollView(this);
        scroll.setPadding(0, Ui.md(this), 0, Ui.md(this));
        scroll.setClipToPadding(false);
        scroll.addView(list);
        overlay.addView(scroll, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f));

        for (PairedDevice device : devices) {
            // Name and address are two levels of information, so they read as
            // two: the laptop's name at full strength, where it was last seen
            // muted and monospaced underneath it.
            Button entry = Ui.rowButton(this, device.name, device.subtitle(), false);
            entry.setOnClickListener(v -> connectToSaved(root, overlay, device));
            entry.setOnLongClickListener(v -> {
                confirmForget(root, overlay, device);
                return true;
            });
            list.addView(entry, Ui.stacked(this, 6));
        }

        Button usbSetup = Ui.button(this, "🔌   Add over USB");
        usbSetup.setOnClickListener(v -> showUsbPairingHelp());
        overlay.addView(usbSetup, Ui.stacked(this, 6));

        Button scanQrButton = Ui.button(this, "📷   Add by scanning QR");
        scanQrButton.setOnClickListener(v ->
                startActivityForResult(new Intent(this, QrScanActivity.class), REQUEST_SCAN_QR));
        overlay.addView(scanQrButton, Ui.stacked(this, 6));

        Button findButton = Ui.button(this, "📡   Find on this network");
        findButton.setOnClickListener(v -> {
            root.removeView(overlay);
            showDiscoveryOverlay(root);
        });
        overlay.addView(findButton, Ui.stacked(this, 0));

        root.addView(overlay, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
    }

    private void connectToSaved(FrameLayout root, LinearLayout overlay, PairedDevice device) {
        DeviceStore.touch(this, device);
        completeConnectionSetup(root, java.util.Collections.singletonList(overlay),
                device.host, device.port, device.token, device.pubkey);
    }

    private void confirmForget(FrameLayout root, LinearLayout overlay, PairedDevice device) {
        new android.app.AlertDialog.Builder(this)
                .setTitle("Forget " + device.name + "?")
                .setMessage("You will need to pair again to reconnect to it.")
                .setNegativeButton("Cancel", null)
                .setPositiveButton("Forget", (d, w) -> {
                    DeviceStore.remove(this, device);
                    root.removeView(overlay);
                    showDeviceListOverlay(root);
                })
                .show();
    }

    /**
     * Explains the USB pairing step, which necessarily runs on the laptop.
     *
     * Being straight about the direction matters here: USB debugging is a
     * protocol by which a computer inspects a phone, never the reverse, so
     * this app cannot detect the laptop, scan for it, or initiate anything
     * over the cable. What it *can* do is show whether the preconditions on
     * this side are met and then react the moment the laptop pushes
     * credentials across -- which is what the Devices list updating amounts
     * to. Presenting that as the app "detecting" the laptop would be a
     * comfortable lie that leaves the user with no idea what to fix when it
     * does not work.
     */
    private void showUsbPairingHelp() {
        boolean debuggingOn = UsbSetupStatus.isAdbEnabled(this);
        boolean cablePlugged = UsbSetupStatus.isUsbConnected(this);

        String message =
                "On this phone\n"
                + (debuggingOn ? "  ✓ USB debugging is ON\n" : "  ✗ USB debugging is OFF\n"
                        + "     Settings → Developer options → USB debugging\n")
                + (cablePlugged ? "  ✓ USB cable connected\n" : "  ✗ No USB cable detected\n")
                + "\nThen, on your laptop\n"
                + "  ./scripts/pair-usb.sh\n"
                + "\nThe laptop does the detecting -- USB debugging only works in that "
                + "direction. This screen updates by itself once it has sent the details "
                + "across.";

        new android.app.AlertDialog.Builder(this)
                .setTitle("Add over USB")
                .setMessage(message)
                .setPositiveButton("OK", null)
                .setNeutralButton("Re-check", (d, w) -> showUsbPairingHelp())
                .show();
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
        overlay.setBackgroundColor(Ui.PANEL);
        int pad = Ui.lg(this);
        overlay.setPadding(Ui.xl(this), pad, Ui.xl(this), pad);
        overlay.setClickable(true);
        discoveryRoot = root;
        discoveryOverlayView = overlay;

        overlay.addView(Ui.title(this, "Find a laptop"), Ui.stacked(this, 12));

        Button scanQrButton = Ui.button(this, "📷   Scan QR code");
        scanQrButton.setOnClickListener(v ->
                startActivityForResult(new Intent(this, QrScanActivity.class), REQUEST_SCAN_QR));
        overlay.addView(scanQrButton, Ui.stacked(this, 12));

        TextView hint = Ui.body(this, "Scanning for palmtopd on this Wi-Fi network…");
        hint.setTextColor(Ui.TEXT_FAINT);
        overlay.addView(hint, Ui.stacked(this, 10));

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
                    Button entry = Ui.rowButton(MainActivity.this, name,
                            foundHost + ":" + foundPort, false);
                    entry.setOnClickListener(v ->
                            promptForTokenAndConnect(root, overlay, foundHost, foundPort, foundPubkey));
                    results.addView(entry, Ui.stacked(MainActivity.this, 6));
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
        overlay.addView(Ui.hairline(this), new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, Ui.dp(this, 1)));
        TextView manualLabel = Ui.body(this, "Or enter the details yourself");
        LinearLayout.LayoutParams labelLp = Ui.stacked(this, 8);
        labelLp.topMargin = Ui.md(this);
        overlay.addView(manualLabel, labelLp);

        EditText manualHost = Ui.input(this, "host  (e.g. 192.168.1.42)");
        overlay.addView(manualHost, Ui.stacked(this, 6));
        EditText manualPort = Ui.input(this, "port  (e.g. 9999)");
        manualPort.setInputType(InputType.TYPE_CLASS_NUMBER);
        overlay.addView(manualPort, Ui.stacked(this, 6));
        EditText manualPubkey = Ui.input(this, "pubkey  (64 hex characters)");
        overlay.addView(manualPubkey, Ui.stacked(this, 10));
        Button manualGo = Ui.primaryButton(this, "Next");
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

        LinearLayout tokenRow = Ui.sheet(this);
        tokenRow.setBackgroundColor(Ui.PANEL);

        tokenRow.addView(Ui.title(this, "Connect to " + foundHost), Ui.stacked(this, 2));
        TextView label = Ui.body(this,
                "Enter the pairing token the laptop printed when palmtopd started.");
        tokenRow.addView(label, Ui.stacked(this, 12));

        EditText tokenInput = Ui.input(this, "pairing token");
        tokenRow.addView(tokenInput, Ui.stacked(this, 14));

        // Pre-filled when mDNS supplied it (the normal case); left editable
        // as a fallback in case an older host doesn't advertise it yet, or
        // discovery didn't resolve TXT records for some reason.
        TextView pubkeyLabel = Ui.body(this, "Host public key — filled in automatically");
        tokenRow.addView(pubkeyLabel, Ui.stacked(this, 6));
        EditText pubkeyInput = Ui.input(this, "pubkey  (64 hex characters)");
        pubkeyInput.setText(foundPubkey == null ? "" : foundPubkey);
        tokenRow.addView(pubkeyInput, Ui.stacked(this, 14));

        Button connectBtn = Ui.primaryButton(this, "Connect");
        tokenRow.addView(connectBtn, Ui.stacked(this, 0));

        root.addView(tokenRow, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        connectBtn.setOnClickListener(v -> {
            String enteredToken = tokenInput.getText().toString().trim();
            String enteredPubkey = pubkeyInput.getText().toString().trim();
            if (enteredPubkey.isEmpty()) {
                pubkeyLabel.setTextColor(Ui.ERR);
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
        // Every pairing path funnels through here, so this is the one
        // place a device needs saving. Keyed on the host's public key, so
        // re-pairing a laptop that changed networks updates it in place
        // rather than leaving a stale duplicate -- see PairedDevice.
        DeviceStore.upsert(this, new PairedDevice(
                h, h, p, t, pk, System.currentTimeMillis()));

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
        SessionLog.startSession();
        SessionLog.info("app", "Palmtop " + appVersion() + ", protocol v" + Protocol.VERSION);
        SessionLog.info("net", "connecting to " + host + ":" + port);
        setStatus(Ui.ACCENT, "connecting to " + host + ":" + port + " ...");
        new Thread(() -> runNetwork(surfaceHolder, myGeneration), "palmtop-net").start();
    }

    /** Version of this build, for the session log. Answering "are the two
     *  sides the same version?" should not require anyone to guess. */
    private String appVersion() {
        try {
            return getPackageManager().getPackageInfo(getPackageName(), 0).versionName;
        } catch (Exception e) {
            return "unknown";
        }
    }

    private void teardown() {
        // Release anything still held before the socket goes. A connection
        // dropped mid-drag would otherwise leave the laptop with a stuck
        // mouse button and no way to clear it from this end.
        for (int b = 0; b < buttonHeld.length; b++) {
            if (buttonHeld[b]) {
                buttonHeld[b] = false;
                if (connected) enqueue(Protocol.pointerButton(b, false));
            }
        }
        if (cursorDriver != null) cursorDriver.stop();
        releaseLatchedModifiers();

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
        // Latched modifiers ride along with every keystroke. The modifier
        // keys themselves are already physically held on the host (see
        // toggleModifier), but the bitmask has to accompany each event too --
        // the host sets the compositor's depressed-modifier state from it on
        // every key, so omitting it here would clear the modifier for exactly
        // the keystroke it was meant to apply to.
        int latched = modifiers.mask();
        if (c == '\b') {
            enqueue(Protocol.key(Keycodes.KEY_BACKSPACE, true, latched));
            enqueue(Protocol.key(Keycodes.KEY_BACKSPACE, false, latched));
            return;
        }
        int[] mapping = Keycodes.lookup(c);
        if (mapping == null) {
            Log.w(TAG, "no keycode mapping for '" + c + "' -- dropped (ASCII only for now)");
            return;
        }
        int code = mapping[0];
        int mods = latched | (mapping[1] != 0 ? Protocol.MOD_SHIFT : 0);
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
            // A connect timeout alone is not enough, and assuming it was hid a
            // real bug for a while: connect() succeeds as soon as the kernel
            // completes the TCP handshake, which it does whether or not the
            // host ever reads the connection. A host that accepted us into its
            // backlog and then never looked at us left this thread blocked in
            // the Noise handshake read forever, showing "connecting..." with no
            // error to act on. A read timeout is what turns that silence into a
            // message. Generous, because the legitimate slow case -- waiting on
            // a human to approve the screen-share dialog -- is covered by the
            // host's Status messages resetting this clock.
            mySocket.setSoTimeout(20000);
            SessionLog.good("net", "TCP connected to " + host + ":" + port);

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

            // Reads the live field rather than trusting the `holder` this thread
            // was started with -- if the surface was recreated in the gap
            // between connecting and here (the same race the very first
            // report of this bug came from), that captured reference would
            // already be pointing at a destroyed surface.
            SurfaceHolder liveHolder = surfaceHolder;
            if (liveHolder == null) {
                throw new IOException("no video surface is currently available");
            }
            configureCodec(liveHolder, cfg.width, cfg.height);
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
                    handleVideoConfigChange(msg, surfaceHolder);
                } else if (msg.tag == Protocol.TAG_STATUS) {
                    handleStatus(msg);
                }
            }
            writer.interrupt();
        } catch (Exception e) {
            Log.e(TAG, "network thread failed", e);
            SessionLog.error("net", String.valueOf(e.getMessage() != null ? e.getMessage() : e));
            // "failed to connect after 10000ms" is true and useless on its own.
            // Far and away the most common cause is that the two devices are
            // not on the same network -- or that the laptop's address has
            // changed since pairing, which looks identical from here. Say which.
            if (e instanceof java.net.SocketTimeoutException
                    || e instanceof java.net.ConnectException
                    || e instanceof java.net.NoRouteToHostException) {
                String why = NetworkCheck.explainUnreachable(host);
                if (why != null) SessionLog.error("net", why);
            }
            if (generation == myGeneration) {
                runOnUiThread(() -> {
                    setStatus(Ui.ERR, "ERROR: " + e + "\n\ntap ⚙ → Reconnect to retry");
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
        SessionLog.good("crypto", "encrypted channel established");

        DeviceProfile profile = deviceProfile;
        if (profile == null) {
            // Should not happen (onCreate detects it), but a null here
            // would abort the connection entirely -- degrading to the
            // conservative profile costs some quality and keeps the
            // session working, which is the better failure.
            profile = DeviceProfile.fallback();
        }
        sendEncrypted(rawOut, Protocol.hello(token, profile));
        Protocol.Received ack = recvEncrypted(rawIn);
        if (ack == null || ack.tag != Protocol.TAG_HELLO_ACK || !ack.ok) {
            String reason = ack != null ? ack.reason : "connection closed during handshake";
            SessionLog.error("pairing", reason);
            throw new IOException("handshake rejected: " + reason);
        }
        SessionLog.good("pairing", "accepted by the laptop");
    }

    /**
     * Waits for the stream's opening VideoConfig, surfacing any Status the
     * host reports while we wait.
     *
     * <p>This deliberately is not "read one message and require VideoConfig".
     * Between the handshake and the first frame, the host has to get the
     * screen-share dialog approved by a human standing at the laptop, and that
     * can take as long as it takes. A v5 host narrates that wait (see
     * palmtop-proto's Status), which is the difference between a phone that
     * looks frozen and one that says "approve the dialog on the laptop" --
     * exactly the confusion reported when the prompt appeared to never come.
     *
     * <p>A failing Status ends the wait immediately with the host's own
     * explanation, rather than leaving the connection hanging until something
     * eventually times out with a generic error.
     */
    private Protocol.Received readInitialVideoConfig(DataInputStream rawIn) throws Exception {
        while (true) {
            Protocol.Received msg = recvEncrypted(rawIn);
            if (msg == null) {
                throw new IOException("host closed the connection before the stream started");
            }
            if (msg.tag == Protocol.TAG_VIDEO_CONFIG) {
                return msg;
            }
            if (msg.tag == Protocol.TAG_STATUS) {
                handleStatus(msg);
                if (!msg.ok) {
                    throw new IOException(msg.detail);
                }
                continue;
            }
            // Anything else here really is out of order.
            throw new IOException("expected VideoConfig, got tag " + msg.tag);
        }
    }

    /** Records a host Status in the session log and reflects the important
     *  ones on screen, so the current state is visible without opening the
     *  log at all. */
    private void handleStatus(Protocol.Received msg) {
        if (msg.ok) {
            SessionLog.good(msg.stage, msg.detail);
        } else {
            SessionLog.error(msg.stage, msg.detail);
        }
        final boolean failed = !msg.ok;
        final String detail = msg.detail;
        runOnUiThread(() -> {
            if (statusView == null) return;
            setStatus(failed ? Ui.ERR : Ui.TEXT_MUTED,
                    failed ? ("ERROR: " + detail) : detail);
        });
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
                // The one moment worth colouring: the stream is actually live.
                setStatus(Ui.OK, "● connected  " + host + ":" + port + "\n"
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
            // Steady state drops back to muted: once the picture is up, the
            // status line is reference information, not an alert.
            setStatus(Ui.TEXT_MUTED, host + ":" + port + "\n"
                    + vc.width + "x" + vc.height + "@" + vc.fps + "fps\n"
                    + "decoded " + d + "   stale " + sk);
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
            // A null holder here means the surface is between destroy and
            // recreate right now -- releasing the old codec (above) is still
            // correct, and surfaceCreated's own rebind path (see
            // wireSurfaceCallbacks) will configure the new one using this
            // videoConfig the moment the surface comes back, so there is
            // nothing to configure against yet rather than something wrong.
            if (holder != null) {
                configureCodec(holder, msg.width, msg.height);
            }
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
