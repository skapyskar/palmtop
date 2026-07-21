package dev.palmtop.spike;

import android.app.Activity;
import android.graphics.Color;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Log;
import android.view.Gravity;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.WindowManager;
import android.widget.FrameLayout;
import android.widget.TextView;

import java.io.DataInputStream;
import java.io.InputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

/**
 * Phase 0 spike: measure MediaCodec hardware decode latency -- the last
 * unmeasured leg of Palmtop's glass-to-glass budget.
 *
 * Receives length-prefixed H.264 access units over TCP, decodes them with the
 * device's low-latency hardware decoder, renders to a SurfaceView, and reports
 * decode latency (queue-input -> output-available) which is measurable purely
 * on-device and so needs no clock sync with the host.
 */
public class MainActivity extends Activity {
    private static final String TAG = "PalmtopSpike";
    private static final String MIME = MediaFormat.MIMETYPE_VIDEO_AVC;

    // Host address is supplied at launch -- never hardcoded, since it differs per
    // machine and per network. scripts/run-decode-spike.sh fills these in from
    // config/host.toml. See config/README.md.
    //   adb shell am start -n dev.palmtop.spike/.MainActivity --es host <ip> --ei port <port>
    private String host = null;
    private int port = 0;
    /**
     * Whether to actually render decoded frames to the surface.
     *
     * Rendering couples decode to display vsync (the codec can stall waiting for
     * output buffers to come back from the display pipeline), so measuring with
     * render off isolates pure decode cost. Override with `--ei render 0`.
     */
    private boolean render = true;
    /**
     * Max frames allowed in the decoder at once.
     *
     * The decoder exposes several input buffers, so feeding it whenever one is
     * free lets a standing queue build -- latency then equals queue_depth/rate
     * rather than actual decode cost. Capping in-flight frames trades a little
     * throughput headroom for a large latency win. Override with `--ei inflight N`.
     */
    private int maxInFlight = 1;
    private java.util.concurrent.Semaphore inFlight;

    private SurfaceView surfaceView;
    private TextView stats;
    private volatile boolean running = true;

    private MediaCodec codec;
    private HandlerThread codecThread;

    /** presentationTimeUs -> System.nanoTime() at queue time. */
    private final ConcurrentHashMap<Long, Long> queuedAt = new ConcurrentHashMap<>();
    /**
     * Indices of input buffers the codec has handed us.
     *
     * MediaCodec delivers every callback on a single handler thread, so the
     * callbacks must never block -- blocking in onInputBufferAvailable stalls
     * onOutputBufferAvailable behind it and corrupts the latency measurement.
     * So the callback only records the index, and the network thread does the
     * (potentially blocking) work of waiting for data and queueing it.
     */
    private final LinkedBlockingQueue<Integer> availableInputs = new LinkedBlockingQueue<>();

    // Latency accumulators (nanoseconds).
    private final Object lock = new Object();
    private long frames = 0;
    private long latSum = 0;
    private long latMin = Long.MAX_VALUE;
    private long latMax = 0;
    private long[] samples = new long[100000];
    private int sampleCount = 0;
    private long firstFrameNs = 0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);

        host = getIntent().getStringExtra("host");
        port = getIntent().getIntExtra("port", 0);
        if (host == null || host.isEmpty() || port == 0) {
            String msg = "No host configured.\n\nLaunch via scripts/run-decode-spike.sh,\n"
                    + "or pass --es host <ip> --ei port <port>.";
            Log.e(TAG, msg.replace('\n', ' '));
            TextView err = new TextView(this);
            err.setTextColor(Color.RED);
            err.setTextSize(16);
            err.setText(msg);
            setContentView(err);
            return;
        }
        render = getIntent().getIntExtra("render", 1) != 0;
        maxInFlight = Math.max(1, getIntent().getIntExtra("inflight", 1));
        inFlight = new java.util.concurrent.Semaphore(maxInFlight);

        FrameLayout root = new FrameLayout(this);
        surfaceView = new SurfaceView(this);
        root.addView(surfaceView, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        stats = new TextView(this);
        stats.setTextColor(Color.GREEN);
        stats.setBackgroundColor(Color.argb(160, 0, 0, 0));
        stats.setTextSize(13);
        stats.setText("connecting to " + host + ":" + port + " ...");
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT);
        lp.gravity = Gravity.TOP;
        root.addView(stats, lp);
        setContentView(root);

        surfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
            private boolean started = false;
            @Override public void surfaceCreated(SurfaceHolder holder) {
                if (!started) {
                    started = true;
                    new Thread(() -> runSpike(holder), "palmtop-net").start();
                }
            }
            @Override public void surfaceChanged(SurfaceHolder h, int f, int w, int ht) {}
            @Override public void surfaceDestroyed(SurfaceHolder h) { running = false; }
        });
    }

    /** Prefer a vendor low-latency decoder if the device advertises one. */
    private String pickDecoder() {
        MediaCodecList list = new MediaCodecList(MediaCodecList.REGULAR_CODECS);
        String fallback = null;
        for (MediaCodecInfo info : list.getCodecInfos()) {
            if (info.isEncoder()) continue;
            for (String type : info.getSupportedTypes()) {
                if (!type.equalsIgnoreCase(MIME)) continue;
                String name = info.getName();
                if (name.contains("low_latency")) {
                    Log.i(TAG, "using vendor low-latency decoder: " + name);
                    return name;
                }
                if (fallback == null && !name.startsWith("OMX.google")
                        && !name.startsWith("c2.android")) {
                    fallback = name; // hardware, but not the low_latency variant
                }
            }
        }
        Log.i(TAG, "using decoder: " + fallback);
        return fallback;
    }

    private void runSpike(SurfaceHolder holder) {
        Socket sock = null;
        try {
            sock = new Socket();
            sock.connect(new InetSocketAddress(host, port), 10000);
            sock.setTcpNoDelay(true); // latency over throughput
            Log.i(TAG, "connected to " + host + ":" + port);

            DataInputStream in = new DataInputStream(sock.getInputStream());

            String decoderName = pickDecoder();
            MediaFormat fmt = MediaFormat.createVideoFormat(MIME, 1920, 1080);
            // The key knob this spike exists to exercise (API 30+).
            fmt.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
            fmt.setInteger(MediaFormat.KEY_PRIORITY, 0); // realtime
            fmt.setInteger("vendor.qti-ext-dec-picture-order.enable", 1);

            codecThread = new HandlerThread("palmtop-codec");
            codecThread.start();
            Handler codecHandler = new Handler(codecThread.getLooper());

            codec = MediaCodec.createByCodecName(decoderName);
            codec.setCallback(new MediaCodec.Callback() {
                @Override
                public void onInputBufferAvailable(MediaCodec mc, int index) {
                    // Must return immediately -- see availableInputs javadoc.
                    availableInputs.offer(index);
                }

                @Override
                public void onOutputBufferAvailable(MediaCodec mc, int index,
                                                    MediaCodec.BufferInfo info) {
                    long now = System.nanoTime();
                    Long q = queuedAt.remove(info.presentationTimeUs);
                    if (q != null) { recordLatency(now - q, now); inFlight.release(); }
                    mc.releaseOutputBuffer(index, render);
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
            Log.i(TAG, "codec started (render=" + render + ")");

            byte[] lenBuf = new byte[4];
            while (running) {
                in.readFully(lenBuf);
                int len = ((lenBuf[0] & 0xff) << 24) | ((lenBuf[1] & 0xff) << 16)
                        | ((lenBuf[2] & 0xff) << 8) | (lenBuf[3] & 0xff);
                if (len <= 0 || len > 8 * 1024 * 1024) {
                    Log.e(TAG, "bad frame length " + len);
                    break;
                }
                byte[] au = new byte[len];
                in.readFully(au);

                // Feed straight through: take a free input buffer and queue the
                // frame immediately. Frames arrive in real time at the source
                // framerate, so there is nothing to gain from buffering ahead --
                // and buffering ahead is exactly what inflates latency.
                // Hold the line on in-flight frames before consuming an input buffer.
                if (!inFlight.tryAcquire(2, TimeUnit.SECONDS)) {
                    Log.w(TAG, "decoder saturated -- dropping frame");
                    continue;
                }
                Integer index = availableInputs.poll(2, TimeUnit.SECONDS);
                if (index == null) {
                    inFlight.release();
                    Log.w(TAG, "no input buffer available -- dropping frame");
                    continue;
                }
                ByteBuffer buf = codec.getInputBuffer(index);
                buf.clear();
                buf.put(au);
                long ptsUs = System.nanoTime() / 1000L;
                queuedAt.put(ptsUs, System.nanoTime());
                codec.queueInputBuffer(index, 0, au.length, ptsUs, 0);
            }
        } catch (Exception e) {
            Log.e(TAG, "spike failed", e);
            runOnUiThread(() -> stats.setText("ERROR: " + e));
        } finally {
            try { if (sock != null) sock.close(); } catch (Exception ignored) {}
            report();
        }
    }

    private void recordLatency(long ns, long now) {
        synchronized (lock) {
            if (firstFrameNs == 0) firstFrameNs = now;
            frames++;
            latSum += ns;
            if (ns < latMin) latMin = ns;
            if (ns > latMax) latMax = ns;
            if (sampleCount < samples.length) samples[sampleCount++] = ns;
            if (frames % 30 == 0) {
                final double avgMs = (latSum / (double) frames) / 1e6;
                final double fps = frames / ((now - firstFrameNs) / 1e9);
                final long f = frames;
                final double minMs = latMin / 1e6, maxMs = latMax / 1e6;
                Log.i(TAG, String.format(
                        "frames=%d decode_avg=%.2fms min=%.2fms max=%.2fms fps=%.1f",
                        f, avgMs, minMs, maxMs, fps));
                runOnUiThread(() -> stats.setText(String.format(
                        "PALMTOP DECODE SPIKE%nframes %d%ndecode avg %.2f ms"
                        + "%nmin %.2f  max %.2f%nrender %.1f fps",
                        f, avgMs, minMs, maxMs, fps)));
            }
        }
    }

    /** Final percentile report -- the numbers that matter for the latency gate. */
    private void report() {
        synchronized (lock) {
            if (frames == 0) { Log.w(TAG, "RESULT no frames decoded"); return; }
            long[] s = new long[sampleCount];
            System.arraycopy(samples, 0, s, 0, sampleCount);
            java.util.Arrays.sort(s);
            Log.i(TAG, String.format(
                    "RESULT frames=%d avg=%.2fms min=%.2fms p50=%.2fms p95=%.2fms p99=%.2fms max=%.2fms",
                    frames,
                    (latSum / (double) frames) / 1e6,
                    latMin / 1e6,
                    s[(int) (s.length * 0.50)] / 1e6,
                    s[(int) (s.length * 0.95)] / 1e6,
                    s[(int) (s.length * 0.99)] / 1e6,
                    latMax / 1e6));
        }
    }

    @Override
    protected void onDestroy() {
        running = false;
        try { if (codec != null) { codec.stop(); codec.release(); } } catch (Exception ignored) {}
        if (codecThread != null) codecThread.quitSafely();
        super.onDestroy();
    }
}
