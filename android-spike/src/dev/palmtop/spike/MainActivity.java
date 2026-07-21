package dev.palmtop.spike;

import android.app.Activity;
import android.content.SharedPreferences;
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
import android.widget.TextView;

import java.io.BufferedInputStream;
import java.io.DataInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.ByteBuffer;
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
 * Evolved from the fixed-stream decode-latency spike (which proved the
 * MediaCodec low-latency + inflight-cap=1 approach at 25ms avg) into the real
 * client speaking palmtop-proto against the live palmtopd daemon, instead of
 * a canned test file.
 */
public class MainActivity extends Activity {
    private static final String TAG = "PalmtopClient";
    private static final String MIME = MediaFormat.MIMETYPE_VIDEO_AVC; // host only speaks h264 so far
    private static final String PREFS = "palmtop";

    private String host;
    private int port;
    /** Pairing secret from the host's QR code -- see palmtopd/src/pairing.rs.
     * No in-app scanner yet (deferred; a real camera+ML Kit feature deserving
     * its own pass), so for now this arrives the same way host/port did
     * before persistence existed: `--es token <token>`, then remembered. */
    private String token = "";

    private SurfaceView surfaceView;
    private SurfaceHolder surfaceHolder;
    private TextView statusView;
    private EditText hiddenInput;
    /** Generation counter: incremented on every (re)connect so a network
     * thread from a *previous* connection attempt can tell it's stale and
     * exit quietly instead of fighting the new one over shared state. */
    private volatile int generation = 0;
    private volatile boolean connected = false;

    private Socket socket;
    private final LinkedBlockingQueue<byte[]> outbox = new LinkedBlockingQueue<>();

    private MediaCodec codec;
    private HandlerThread codecThread;
    private final LinkedBlockingQueue<Integer> availableInputs = new LinkedBlockingQueue<>();
    /** Caps frames in flight -- the change that took decode latency from ~40ms to 25ms
     * in the Phase 0 spike; same principle applied here from day one. */
    private final Semaphore inFlight = new Semaphore(1);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);

        // Falls back to the last-used host/port when the launching Intent has
        // none -- reopening via the launcher icon (e.g. after pressing back)
        // sends a bare ACTION_MAIN Intent with no extras, which otherwise
        // meant every relaunch needed a fresh `adb shell am start --es host
        // ...` from the host machine just to reconnect.
        SharedPreferences prefs = getSharedPreferences(PREFS, MODE_PRIVATE);
        host = getIntent().getStringExtra("host");
        port = getIntent().getIntExtra("port", 0);
        token = getIntent().getStringExtra("token");
        if (host == null || host.isEmpty() || port == 0) {
            host = prefs.getString("host", null);
            port = prefs.getInt("port", 0);
            token = prefs.getString("token", "");
        } else {
            token = token == null ? "" : token;
            prefs.edit().putString("host", host).putInt("port", port).putString("token", token).apply();
        }

        FrameLayout root = new FrameLayout(this);

        surfaceView = new SurfaceView(this);
        root.addView(surfaceView, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        statusView = new TextView(this);
        statusView.setTextColor(Color.GREEN);
        statusView.setBackgroundColor(Color.argb(160, 0, 0, 0));
        statusView.setTextSize(12);
        FrameLayout.LayoutParams statusLp = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT);
        statusLp.gravity = Gravity.TOP;
        root.addView(statusView, statusLp);

        // Invisible-but-focusable EditText: the simplest reliable way to
        // capture typed text regardless of whether a given IME dispatches
        // discrete KeyEvents or batches via commitText (many do the latter,
        // especially with autocorrect) -- diffing the text content works
        // either way. See Keycodes.java for why this stays ASCII-only for now.
        hiddenInput = new EditText(this);
        hiddenInput.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        FrameLayout.LayoutParams inputLp = new FrameLayout.LayoutParams(1, 1);
        root.addView(hiddenInput, inputLp);
        hiddenInput.addTextChangedListener(new TextWatcher() {
            @Override public void beforeTextChanged(CharSequence s, int a, int b, int c) {}
            @Override public void onTextChanged(CharSequence s, int a, int b, int c) {}
            @Override public void afterTextChanged(Editable s) {
                for (int i = 0; i < s.length(); i++) {
                    sendChar(s.charAt(i));
                }
                s.clear(); // reset baseline so we only ever see newly-typed chars
            }
        });

        Button kbToggle = new Button(this);
        kbToggle.setText("⌨");
        FrameLayout.LayoutParams kbLp = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT);
        kbLp.gravity = Gravity.BOTTOM | Gravity.END;
        root.addView(kbToggle, kbLp);
        kbToggle.setOnClickListener(v -> {
            hiddenInput.requestFocus();
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            if (imm != null) imm.showSoftInput(hiddenInput, 0);
        });

        // Always-available escape hatch: retry the connection in place
        // (network hiccup, host restarted, portal dialog dismissed by
        // mistake, ...) without needing another adb-launched Intent.
        Button reconnect = new Button(this);
        reconnect.setText("⟳ Reconnect");
        FrameLayout.LayoutParams reconnectLp = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT);
        reconnectLp.gravity = Gravity.BOTTOM | Gravity.START;
        root.addView(reconnect, reconnectLp);
        reconnect.setOnClickListener(v -> startConnection());

        setContentView(root);

        if (host == null || host.isEmpty() || port == 0) {
            statusView.setTextColor(Color.RED);
            statusView.setText("No host configured yet.\nLaunch once via scripts/run-client.sh "
                    + "(or --es host <ip> --ei port <port>);\nit's remembered after that.");
            return;
        }

        surfaceView.setOnTouchListener(this::onTouch);
        surfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override public void surfaceCreated(SurfaceHolder holder) {
                surfaceHolder = holder;
                startConnection();
            }
            @Override public void surfaceChanged(SurfaceHolder h, int f, int w, int ht) {}
            @Override public void surfaceDestroyed(SurfaceHolder h) { generation++; }
        });
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
        outbox.clear();
        try { if (socket != null) socket.close(); } catch (IOException ignored) {}
        socket = null;
        try { if (codec != null) { codec.stop(); codec.release(); } } catch (Exception ignored) {}
        codec = null;
        if (codecThread != null) codecThread.quitSafely();
        codecThread = null;
        availableInputs.clear();
        inFlight.drainPermits();
        inFlight.release();
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
     */
    private boolean onTouch(View v, MotionEvent event) {
        if (!connected) return true;
        int w = surfaceView.getWidth();
        int h = surfaceView.getHeight();
        if (w == 0 || h == 0) return true;
        float nx = event.getX() / w;
        float ny = event.getY() / h;

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

            OutputStream rawOut = mySocket.getOutputStream();
            rawOut.write(Protocol.hello(token));
            rawOut.flush();

            DataInputStream in = new DataInputStream(new BufferedInputStream(mySocket.getInputStream()));
            Protocol.Received ack = Protocol.readMessage(in);
            if (ack == null || ack.tag != Protocol.TAG_HELLO_ACK || !ack.ok) {
                String reason = ack != null ? ack.reason : "connection closed during handshake";
                throw new IOException("handshake rejected: " + reason);
            }
            Log.i(TAG, "handshake ok");

            Protocol.Received cfg = Protocol.readMessage(in);
            if (cfg == null || cfg.tag != Protocol.TAG_VIDEO_CONFIG) {
                throw new IOException("expected VideoConfig, got " + (cfg == null ? "EOF" : cfg.tag));
            }
            Log.i(TAG, "video config: " + cfg.codec + " " + cfg.width + "x" + cfg.height + "@" + cfg.fps);
            if (generation == myGeneration) {
                runOnUiThread(() -> statusView.setText("connected " + host + ":" + port + "\n"
                        + cfg.width + "x" + cfg.height + "@" + cfg.fps + "fps"));
            }

            configureCodec(holder, cfg.width, cfg.height);
            connected = true;

            Thread writer = new Thread(() -> runWriter(rawOut, myGeneration), "palmtop-writer");
            writer.start();

            while (generation == myGeneration) {
                Protocol.Received msg = Protocol.readMessage(in);
                if (msg == null) {
                    Log.i(TAG, "host closed the connection");
                    break;
                }
                if (msg.tag == Protocol.TAG_VIDEO_FRAME) {
                    feedDecoder(msg.data);
                } else if (msg.tag == Protocol.TAG_PING) {
                    enqueue(Protocol.pong(msg.nonce));
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

    private void runWriter(OutputStream out, int myGeneration) {
        try {
            while (generation == myGeneration) {
                byte[] msg = outbox.poll(1, TimeUnit.SECONDS);
                if (msg == null) continue;
                out.write(msg);
                out.flush();
            }
        } catch (Exception e) {
            Log.i(TAG, "writer thread stopping: " + e);
        }
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
        codec.setCallback(new MediaCodec.Callback() {
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
                inFlight.release();
                mc.releaseOutputBuffer(index, true);
            }
            @Override public void onError(MediaCodec mc, MediaCodec.CodecException e) {
                Log.e(TAG, "codec error", e);
            }
            @Override public void onOutputFormatChanged(MediaCodec mc, MediaFormat f) {
                Log.i(TAG, "output format: " + f);
            }
        }, codecHandler);

        codec.configure(fmt, holder.getSurface(), null, 0);
        codec.start();
    }

    private void feedDecoder(byte[] au) {
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
            codec.queueInputBuffer(index, 0, au.length, System.nanoTime() / 1000L, 0);
        } catch (Exception e) {
            Log.e(TAG, "feedDecoder", e);
        }
    }

    @Override
    protected void onDestroy() {
        generation++; // orphans any in-flight network/writer threads
        teardown();
        super.onDestroy();
    }
}
