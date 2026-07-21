package dev.palmtop.spike;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;

/**
 * Java implementation of the wire protocol defined in {@code crates/palmtop-proto}.
 * Framing: [1-byte tag][4-byte BE length][payload]. Java's DataInput/DataOutputStream
 * are big-endian by default, matching Rust's {@code to_be_bytes}/{@code from_be_bytes}
 * exactly -- no manual byte-order handling needed on either side.
 */
public final class Protocol {
    /** v2: Hello gained a pairing `token` field -- see palmtopd/src/pairing.rs. */
    public static final int VERSION = 2;
    private static final int MAX_PAYLOAD = 16 * 1024 * 1024;

    public static final int TAG_HELLO = 1;
    public static final int TAG_HELLO_ACK = 2;
    public static final int TAG_VIDEO_CONFIG = 3;
    public static final int TAG_VIDEO_FRAME = 4;
    public static final int TAG_POINTER_MOTION_RELATIVE = 5;
    public static final int TAG_POINTER_MOTION_ABSOLUTE = 6;
    public static final int TAG_POINTER_BUTTON = 7;
    public static final int TAG_SCROLL = 8;
    public static final int TAG_KEY = 9;
    public static final int TAG_TEXT = 10;
    public static final int TAG_PING = 11;
    public static final int TAG_PONG = 12;

    public static final int BUTTON_LEFT = 0;
    public static final int BUTTON_RIGHT = 1;
    public static final int BUTTON_MIDDLE = 2;

    public static final int MOD_SHIFT = 1;
    public static final int MOD_CTRL = 1 << 1;
    public static final int MOD_ALT = 1 << 2;
    public static final int MOD_SUPER = 1 << 3;

    private Protocol() {}

    // ---- outgoing (client -> host); each returns a fully-framed message ----

    public static byte[] hello(String token) {
        return frame(TAG_HELLO, p -> {
            p.writeShort(VERSION);
            writeString(p, token);
        });
    }

    public static byte[] pointerMotionRelative(float dx, float dy) {
        return frame(TAG_POINTER_MOTION_RELATIVE, p -> {
            p.writeFloat(dx);
            p.writeFloat(dy);
        });
    }

    public static byte[] pointerMotionAbsolute(float x, float y) {
        return frame(TAG_POINTER_MOTION_ABSOLUTE, p -> {
            p.writeFloat(x);
            p.writeFloat(y);
        });
    }

    public static byte[] pointerButton(int button, boolean pressed) {
        return frame(TAG_POINTER_BUTTON, p -> {
            p.writeByte(button);
            p.writeByte(pressed ? 1 : 0);
        });
    }

    public static byte[] scroll(float dx, float dy) {
        return frame(TAG_SCROLL, p -> {
            p.writeFloat(dx);
            p.writeFloat(dy);
        });
    }

    public static byte[] key(int evdevCode, boolean pressed, int modifiers) {
        return frame(TAG_KEY, p -> {
            p.writeInt(evdevCode);
            p.writeByte(pressed ? 1 : 0);
            p.writeByte(modifiers);
        });
    }

    public static byte[] text(String utf8) {
        return frame(TAG_TEXT, p -> writeString(p, utf8));
    }

    public static byte[] ping(long nonce) {
        return frame(TAG_PING, p -> p.writeLong(nonce));
    }

    public static byte[] pong(long nonce) {
        return frame(TAG_PONG, p -> p.writeLong(nonce));
    }

    private interface PayloadWriter {
        void write(DataOutputStream p) throws IOException;
    }

    private static byte[] frame(int tag, PayloadWriter body) {
        try {
            ByteArrayOutputStream payloadBuf = new ByteArrayOutputStream();
            body.write(new DataOutputStream(payloadBuf));
            byte[] payload = payloadBuf.toByteArray();

            ByteArrayOutputStream out = new ByteArrayOutputStream(5 + payload.length);
            DataOutputStream dout = new DataOutputStream(out);
            dout.writeByte(tag);
            dout.writeInt(payload.length);
            dout.write(payload);
            return out.toByteArray();
        } catch (IOException e) {
            // Writing to in-memory buffers cannot fail.
            throw new RuntimeException(e);
        }
    }

    private static void writeString(DataOutputStream p, String s) throws IOException {
        byte[] b = s.getBytes(StandardCharsets.UTF_8);
        p.writeInt(b.length);
        p.write(b);
    }

    // ---- incoming (host -> client) ----

    /** Tagged-union-style POJO; only the fields relevant to {@link #tag} are populated. */
    public static final class Received {
        public int tag;
        public boolean ok;
        public String reason;
        public String codec;
        public int width, height, fps;
        public boolean keyframe;
        public byte[] data;
        public long nonce;
    }

    /** Blocks for one full message. Returns null on a clean EOF at a message boundary. */
    public static Received readMessage(DataInputStream in) throws IOException {
        int tag;
        try {
            tag = in.readUnsignedByte();
        } catch (java.io.EOFException e) {
            return null;
        }
        int len = in.readInt();
        if (len < 0 || len > MAX_PAYLOAD) {
            throw new IOException("payload length " + len + " exceeds max -- corrupt stream?");
        }
        byte[] payload = new byte[len];
        in.readFully(payload);
        DataInputStream p = new DataInputStream(new ByteArrayInputStream(payload));

        Received r = new Received();
        r.tag = tag;
        switch (tag) {
            case TAG_HELLO_ACK:
                r.ok = p.readUnsignedByte() != 0;
                r.reason = readString(p);
                break;
            case TAG_VIDEO_CONFIG:
                r.codec = readString(p);
                r.width = p.readInt();
                r.height = p.readInt();
                r.fps = p.readInt();
                break;
            case TAG_VIDEO_FRAME:
                r.keyframe = p.readUnsignedByte() != 0;
                r.data = new byte[payload.length - 1];
                System.arraycopy(payload, 1, r.data, 0, r.data.length);
                break;
            case TAG_PING:
                r.nonce = p.readLong();
                break;
            case TAG_PONG:
                r.nonce = p.readLong();
                break;
            default:
                // Unknown/unhandled message from the host -- ignore the payload.
                break;
        }
        return r;
    }

    private static String readString(DataInputStream in) throws IOException {
        int len = in.readInt();
        byte[] b = new byte[len];
        in.readFully(b);
        return new String(b, StandardCharsets.UTF_8);
    }
}
