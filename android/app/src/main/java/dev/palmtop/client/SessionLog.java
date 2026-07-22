package dev.palmtop.client;

import android.util.Log;

import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/**
 * A short, in-memory record of what this session actually did, readable from
 * inside the app.
 *
 * <p>Everything here used to go only to {@code Log.i}/{@code Log.e}, which is
 * to say: to a developer with a USB cable and adb. The person actually holding
 * the phone saw a blank screen and had no way to tell "waiting for someone to
 * approve a dialog on the laptop" from "the laptop's GPU cannot encode" from
 * "the network dropped". Both failures were reported that way -- a black screen
 * with nothing to act on -- so the log has to be somewhere the user can reach
 * without tooling.
 *
 * <p>Deliberately bounded and deliberately not persisted. This answers "what is
 * happening right now, and what went wrong", which is a question about the
 * current session; keeping history across runs would turn it into a different,
 * larger feature (and a place for pairing tokens to accumulate on disk).
 *
 * <p>Thread-safe: entries arrive from the network thread, the decoder callback
 * and the UI thread, and are read from the UI thread.
 */
public final class SessionLog {

    /** Enough to cover a full connection attempt plus the run-up to a failure,
     *  without letting a chatty session grow without bound. */
    private static final int MAX_ENTRIES = 200;
    private static final String TAG = "palmtop";

    public enum Level { INFO, GOOD, WARN, ERROR }

    public static final class Entry {
        public final long atMs;
        public final Level level;
        public final String stage;
        public final String message;

        Entry(long atMs, Level level, String stage, String message) {
            this.atMs = atMs;
            this.level = level;
            this.stage = stage;
            this.message = message;
        }
    }

    private static final List<Entry> ENTRIES = new ArrayList<>();
    private static long sessionStartMs = System.currentTimeMillis();
    private static Runnable listener;

    private SessionLog() {}

    /** Called when a fresh connection attempt begins, so timings in the log
     *  read as "since this attempt started" rather than since app launch. */
    public static synchronized void startSession() {
        ENTRIES.clear();
        sessionStartMs = System.currentTimeMillis();
        add(Level.INFO, "app", "connection attempt started");
    }

    public static void info(String stage, String message)  { add(Level.INFO, stage, message); }
    public static void good(String stage, String message)  { add(Level.GOOD, stage, message); }
    public static void warn(String stage, String message)  { add(Level.WARN, stage, message); }
    public static void error(String stage, String message) { add(Level.ERROR, stage, message); }

    public static synchronized void add(Level level, String stage, String message) {
        ENTRIES.add(new Entry(System.currentTimeMillis(), level, stage, message));
        while (ENTRIES.size() > MAX_ENTRIES) {
            ENTRIES.remove(0);
        }
        // Mirrored to logcat as well, so an adb-equipped developer loses
        // nothing by this existing -- it adds a channel rather than moving one.
        if (level == Level.ERROR) {
            Log.e(TAG, "[" + stage + "] " + message);
        } else {
            Log.i(TAG, "[" + stage + "] " + message);
        }
        if (listener != null) listener.run();
    }

    /** Notified whenever an entry is appended, so an open log view can follow
     *  a session live instead of showing a snapshot from when it was opened. */
    public static synchronized void setListener(Runnable r) {
        listener = r;
    }

    public static synchronized List<Entry> snapshot() {
        return new ArrayList<>(ENTRIES);
    }

    /** Seconds since this connection attempt began -- the useful frame of
     *  reference when reading "how long did it sit waiting for the portal". */
    public static String stamp(Entry e) {
        double secs = (e.atMs - sessionStartMs) / 1000.0;
        return String.format(Locale.US, "%6.2fs", secs);
    }

    /** The whole log as text, for sharing into a bug report. */
    public static synchronized String asText() {
        StringBuilder sb = new StringBuilder();
        for (Entry e : ENTRIES) {
            sb.append(stamp(e)).append("  ")
              .append(e.level == Level.ERROR ? "FAIL " : e.level == Level.WARN ? "WARN " : "     ")
              .append('[').append(e.stage).append("] ")
              .append(e.message).append('\n');
        }
        return sb.toString();
    }
}
