package dev.palmtop.spike;

import com.southernstorm.noise.protocol.CipherState;
import com.southernstorm.noise.protocol.CipherStatePair;
import com.southernstorm.noise.protocol.HandshakeState;

import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;

/**
 * Java mirror of palmtop-proto's Rust {@code noise} module -- must stay
 * wire-compatible byte-for-byte, since this talks directly to palmtopd's
 * Rust implementation over the same socket. See that module's doc comment
 * for the pattern choice (Noise_NK) and the trust-model caveat (the host's
 * public key is currently learned via mDNS/manual entry, not a scanned-only
 * source -- see MainActivity's discovery flow).
 *
 * Also mirrors the lock-scoping lesson learned the hard way on the Rust
 * side: {@link #encryptChunk}/{@link #decryptChunk} are pure, fast,
 * non-blocking crypto -- safe to guard with a lock shared between a reader
 * and a writer thread. The blocking socket I/O must happen *outside* that
 * lock. The first version of palmtopd's session.rs got this wrong (one
 * combined send/recv call did crypto *and* blocking I/O under one lock) and
 * deadlocked the reader and writer threads against each other -- a blocked
 * read held the lock the writer needed just to send `VideoConfig`. Built
 * split from the start here specifically to not repeat that.
 */
public class NoiseTransport {
    private static final String PATTERN = "Noise_NK_25519_ChaChaPoly_BLAKE2s";
    private static final int NOISE_MSG_MAX = 65535;
    private static final int CHUNK_MAX = 60_000;

    private final CipherState sender;
    private final CipherState receiver;

    private NoiseTransport(CipherState sender, CipherState receiver) {
        this.sender = sender;
        this.receiver = receiver;
    }

    /** Client role: no static keypair of our own (the "N" in NK) -- only
     * needs the host's already-known public key. */
    public static NoiseTransport handshakeInitiator(InputStream in, OutputStream out, byte[] hostPublicKey)
            throws Exception {
        HandshakeState hs = new HandshakeState(PATTERN, HandshakeState.INITIATOR);
        hs.getRemotePublicKey().setPublicKey(hostPublicKey, 0);
        hs.start();
        runHandshake(hs, in, out);
        CipherStatePair pair = hs.split();
        return new NoiseTransport(pair.getSender(), pair.getReceiver());
    }

    /** NK is exactly [initiator write, responder write] -- driven generically
     * off getAction() rather than hardcoding that order, matching the Rust
     * side's `is_my_turn()`-based loop. */
    private static void runHandshake(HandshakeState hs, InputStream in, OutputStream out) throws Exception {
        while (true) {
            int action = hs.getAction();
            if (action == HandshakeState.WRITE_MESSAGE) {
                byte[] buf = new byte[NOISE_MSG_MAX];
                int len = hs.writeMessage(buf, 0, null, 0, 0);
                writeHandshakeFrame(out, buf, len);
            } else if (action == HandshakeState.READ_MESSAGE) {
                byte[] cbuf = readHandshakeFrame(in);
                byte[] pbuf = new byte[cbuf.length];
                hs.readMessage(cbuf, 0, cbuf.length, pbuf, 0);
            } else {
                return; // SPLIT or COMPLETE -- handshake done
            }
        }
    }

    /** Pure crypto, no I/O -- see class doc comment. */
    public synchronized byte[] encryptChunk(byte[] plaintextChunk) throws Exception {
        byte[] out = new byte[NOISE_MSG_MAX];
        int n = sender.encryptWithAd(null, plaintextChunk, 0, out, 0, plaintextChunk.length);
        byte[] result = new byte[n];
        System.arraycopy(out, 0, result, 0, n);
        return result;
    }

    /** Pure crypto, no I/O -- see class doc comment. */
    public synchronized byte[] decryptChunk(byte[] ciphertextChunk) throws Exception {
        byte[] out = new byte[ciphertextChunk.length];
        int n = receiver.decryptWithAd(null, ciphertextChunk, 0, out, 0, ciphertextChunk.length);
        byte[] result = new byte[n];
        System.arraycopy(out, 0, result, 0, n);
        return result;
    }

    /**
     * Splits and encrypts one logical payload into wire-ready frames
     * (mirrors {@code palmtop_proto::noise::chunk_and_encrypt}). The 4-byte
     * total-length header travels inside the *first encrypted chunk*, not as
     * separate plaintext, so an eavesdropper can't learn message boundaries.
     */
    public List<byte[]> chunkAndEncrypt(byte[] plaintext) throws Exception {
        byte[] logical = new byte[4 + plaintext.length];
        writeBE32(logical, 0, plaintext.length);
        System.arraycopy(plaintext, 0, logical, 4, plaintext.length);

        List<byte[]> frames = new ArrayList<>();
        for (int off = 0; off < logical.length; off += CHUNK_MAX) {
            int len = Math.min(CHUNK_MAX, logical.length - off);
            byte[] chunk = new byte[len];
            System.arraycopy(logical, off, chunk, 0, len);
            byte[] ciphertext = encryptChunk(chunk);
            byte[] framed = new byte[4 + ciphertext.length];
            writeBE32(framed, 0, ciphertext.length);
            System.arraycopy(ciphertext, 0, framed, 4, ciphertext.length);
            frames.add(framed);
        }
        return frames;
    }

    /** Blocking read of one wire frame ([4-byte length][ciphertext]). Call
     * this OUTSIDE the crypto lock -- see class doc comment. Returns null on
     * a clean EOF at a frame boundary. */
    public static byte[] readOneFrame(DataInputStream in) throws IOException {
        int len;
        try {
            len = in.readInt();
        } catch (EOFException e) {
            return null;
        }
        if (len <= 0 || len > NOISE_MSG_MAX) {
            throw new IOException("noise frame length " + len + " out of range");
        }
        byte[] buf = new byte[len];
        in.readFully(buf);
        return buf;
    }

    /**
     * Reassembles decrypted chunks back into one logical payload (mirrors
     * {@code palmtop_proto::noise::Reassembler}). One instance per in-flight
     * logical message on the receive side; not shared across threads.
     */
    public static class Reassembler {
        private Integer totalLen = null;
        private final ByteArrayOutputStream acc = new ByteArrayOutputStream();

        /** Returns the complete payload once enough chunks have arrived, else null. */
        public byte[] push(byte[] plaintextChunk) throws IOException {
            if (totalLen == null) {
                if (plaintextChunk.length < 4) {
                    throw new IOException("first noise chunk too short to carry the length header");
                }
                totalLen = readBE32(plaintextChunk, 0);
                acc.write(plaintextChunk, 4, plaintextChunk.length - 4);
            } else {
                acc.write(plaintextChunk, 0, plaintextChunk.length);
            }
            if (acc.size() >= totalLen) {
                byte[] result = acc.toByteArray();
                if (result.length == totalLen) return result;
                byte[] trimmed = new byte[totalLen];
                System.arraycopy(result, 0, trimmed, 0, totalLen);
                return trimmed;
            }
            return null;
        }
    }

    private static void writeHandshakeFrame(OutputStream out, byte[] buf, int len) throws IOException {
        byte[] header = new byte[4];
        writeBE32(header, 0, len);
        out.write(header);
        out.write(buf, 0, len);
        out.flush();
    }

    private static byte[] readHandshakeFrame(InputStream in) throws IOException {
        DataInputStream din = in instanceof DataInputStream ? (DataInputStream) in : new DataInputStream(in);
        int len = din.readInt();
        if (len <= 0 || len > NOISE_MSG_MAX) {
            throw new IOException("handshake frame length " + len + " out of range");
        }
        byte[] buf = new byte[len];
        din.readFully(buf);
        return buf;
    }

    private static void writeBE32(byte[] dst, int offset, int value) {
        dst[offset] = (byte) (value >>> 24);
        dst[offset + 1] = (byte) (value >>> 16);
        dst[offset + 2] = (byte) (value >>> 8);
        dst[offset + 3] = (byte) value;
    }

    private static int readBE32(byte[] src, int offset) {
        return ((src[offset] & 0xff) << 24) | ((src[offset + 1] & 0xff) << 16)
                | ((src[offset + 2] & 0xff) << 8) | (src[offset + 3] & 0xff);
    }
}
