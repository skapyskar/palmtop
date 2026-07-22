//! Transport encryption via the Noise Protocol Framework.
//!
//! Pattern: `Noise_NK_25519_ChaChaPoly_BLAKE2s`. "NK" means the initiator
//! (client) needs **no** static keypair of its own, and the responder's
//! (host's) static public key must already be **k**nown to the initiator
//! ahead of time -- exactly the plan's §3.4/§6 model: the host's pubkey
//! travels in the QR/pairing info (see palmtopd/src/pairing.rs), and the
//! client TOFU-pins it. Client authentication is handled one layer up, by
//! the pairing `token` already carried in `Message::Hello` -- Noise here is
//! purely about encrypting the channel, not about who the client is.
//!
//! NK is a 2-message handshake (`-> e, es` / `<- e, ee`), then both sides
//! move to transport mode. A single Noise transport message is capped at
//! 65535 bytes total (protocol-level limit, not our choice), which video
//! keyframes can exceed -- see `send`/`recv` for the chunking scheme that
//! works around it.

use std::io::{Read, Write};

use anyhow::{bail, Context, Result};
use snow::params::NoiseParams;
use snow::{Builder, HandshakeState, TransportState};

const NOISE_PATTERN: &str = "Noise_NK_25519_ChaChaPoly_BLAKE2s";
/// Hard protocol ceiling (u16). Every wire-framed ciphertext chunk stays under this.
const NOISE_MSG_MAX: usize = 65535;
/// Plaintext chunk size, leaving headroom under NOISE_MSG_MAX for the auth tag
/// and the 4-byte length-header chunk (see `send`).
const CHUNK_MAX: usize = 60_000;

pub struct NoiseTransport {
    state: TransportState,
}

/// Lowercase hex -- used for persisting raw key bytes in `config/host.toml`
/// (TOML has no native bytes type) and for the same bytes in the QR/pairing
/// URI. No external crate: this is small enough not to be worth one, and
/// it's the same pattern `palmtop-config`'s pairing-token generator already
/// used before this module existed.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!("hex string has odd length");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("invalid hex digit"))
        .collect()
}

impl NoiseTransport {
    /// Generates a fresh static X25519 keypair. Called once, on first run --
    /// see `HostConfig::load`'s pairing-token generation for the same pattern.
    pub fn generate_keypair() -> Result<(Vec<u8>, Vec<u8>)> {
        let params = params()?;
        let kp = Builder::new(params).generate_keypair().context("generate noise keypair")?;
        Ok((kp.private, kp.public))
    }

    pub fn handshake_responder<S: Read + Write>(
        stream: &mut S,
        local_private_key: &[u8],
    ) -> Result<Self> {
        let mut hs = Builder::new(params()?)
            .local_private_key(local_private_key)
            .context("set local private key")?
            .build_responder()
            .context("build responder handshake")?;
        run_handshake(stream, &mut hs)?;
        Ok(Self { state: hs.into_transport_mode().context("enter transport mode")? })
    }

    pub fn handshake_initiator<S: Read + Write>(
        stream: &mut S,
        remote_public_key: &[u8],
    ) -> Result<Self> {
        let mut hs = Builder::new(params()?)
            .remote_public_key(remote_public_key)
            .context("set remote (host) public key")?
            .build_initiator()
            .context("build initiator handshake")?;
        run_handshake(stream, &mut hs)?;
        Ok(Self { state: hs.into_transport_mode().context("enter transport mode")? })
    }

    /// Encrypts one already-chunked plaintext piece. Pure computation, no
    /// I/O -- safe to call while holding a lock shared with another thread,
    /// *unlike* [`send`](Self::send)/[`recv`](Self::recv), which do blocking
    /// network I/O internally and must never be called while holding a lock
    /// another thread needs (see `palmtopd/src/session.rs`'s `send_encrypted`/
    /// `recv_encrypted` for why: a shared `Mutex<NoiseTransport>` guarded by
    /// the all-in-one `send`/`recv` deadlocked the reader and writer threads
    /// against each other in exactly this way -- the reader held the lock for
    /// the full duration of a blocking read waiting on the client, starving
    /// the writer of any chance to send `VideoConfig`. Found live, not in
    /// review: the test client hung indefinitely past its own timeout, which
    /// was the tell that something was blocked on I/O it should never have
    /// been blocked on.
    pub fn encrypt_chunk(&mut self, plaintext_chunk: &[u8]) -> Result<Vec<u8>> {
        let mut out = vec![0u8; NOISE_MSG_MAX];
        let n = self.state.write_message(plaintext_chunk, &mut out).context("noise encrypt")?;
        out.truncate(n);
        Ok(out)
    }

    /// Decrypts one wire-framed ciphertext chunk. Pure computation, no I/O --
    /// see [`encrypt_chunk`](Self::encrypt_chunk)'s doc comment.
    pub fn decrypt_chunk(&mut self, ciphertext_chunk: &[u8]) -> Result<Vec<u8>> {
        let mut out = vec![0u8; ciphertext_chunk.len()]; // plaintext is always <= ciphertext length
        let n = self.state.read_message(ciphertext_chunk, &mut out).context("noise decrypt")?;
        out.truncate(n);
        Ok(out)
    }

    /// Encrypts and sends one logical plaintext payload, transparently split
    /// across multiple Noise transport messages if it exceeds the protocol's
    /// per-message limit. The 4-byte total-length header travels inside the
    /// *first* encrypted chunk (not as separate plaintext on the wire) so an
    /// eavesdropper can't even learn the logical message boundaries.
    ///
    /// Single-threaded callers only (e.g. `palmtop-test-client`) -- combines
    /// crypto and blocking I/O in one call, which is exactly what a multi-
    /// threaded caller sharing one `NoiseTransport` behind a lock must avoid.
    /// Use `encrypt_chunk`/`decrypt_chunk` directly there instead.
    pub fn send<S: Write>(&mut self, stream: &mut S, plaintext: &[u8]) -> Result<()> {
        for framed in chunk_and_encrypt(self, plaintext)? {
            stream.write_all(&framed)?;
        }
        Ok(())
    }

    /// Blocks for one full logical payload (possibly reassembled from several
    /// wire chunks). Returns `Ok(None)` on a clean EOF at a chunk boundary.
    pub fn recv<S: Read>(&mut self, stream: &mut S) -> Result<Option<Vec<u8>>> {
        let mut reassembler = Reassembler::new();
        loop {
            let Some(cbuf) = read_one_frame(stream)? else { return Ok(None) };
            let pbuf = self.decrypt_chunk(&cbuf)?;
            if let Some(complete) = reassembler.push(&pbuf)? {
                return Ok(Some(complete));
            }
        }
    }
}

/// Splits `plaintext` (prefixed with its own 4-byte length, so the receiver
/// can tell where the logical message ends across possibly several chunks)
/// into `CHUNK_MAX`-sized pieces, encrypts each, and returns each as a
/// complete wire frame (`[4-byte ciphertext length][ciphertext]`) ready to
/// write. Shared by `send` and by `session.rs`'s lock-scoped equivalent.
pub fn chunk_and_encrypt(transport: &mut NoiseTransport, plaintext: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut logical = Vec::with_capacity(4 + plaintext.len());
    logical.extend_from_slice(&(plaintext.len() as u32).to_be_bytes());
    logical.extend_from_slice(plaintext);

    logical
        .chunks(CHUNK_MAX)
        .map(|chunk| {
            let ciphertext = transport.encrypt_chunk(chunk)?;
            let mut framed = Vec::with_capacity(4 + ciphertext.len());
            framed.extend_from_slice(&(ciphertext.len() as u32).to_be_bytes());
            framed.extend_from_slice(&ciphertext);
            Ok(framed)
        })
        .collect()
}

/// Reads one `[4-byte length][ciphertext]` wire frame. `Ok(None)` means a
/// clean EOF right at a frame boundary.
pub fn read_one_frame<S: Read>(stream: &mut S) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("read noise frame length"),
    }
    let clen = u32::from_be_bytes(len_buf) as usize;
    if clen == 0 || clen > NOISE_MSG_MAX {
        bail!("noise ciphertext frame length {clen} out of range");
    }
    let mut cbuf = vec![0u8; clen];
    stream.read_exact(&mut cbuf).context("read noise ciphertext")?;
    Ok(Some(cbuf))
}

/// Reassembles decrypted chunks (each already stripped of Noise's own
/// framing by `decrypt_chunk`) back into one logical payload, using the
/// 4-byte length header `chunk_and_encrypt` embeds in the first chunk.
pub struct Reassembler {
    total_len: Option<u32>,
    acc: Vec<u8>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self { total_len: None, acc: Vec::new() }
    }

    /// Feed one decrypted plaintext chunk. Returns the complete payload once
    /// enough chunks have arrived, `None` if more are still needed.
    pub fn push(&mut self, plaintext_chunk: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.total_len {
            None => {
                if plaintext_chunk.len() < 4 {
                    bail!("first noise chunk too short to carry the length header");
                }
                self.total_len = Some(u32::from_be_bytes([
                    plaintext_chunk[0],
                    plaintext_chunk[1],
                    plaintext_chunk[2],
                    plaintext_chunk[3],
                ]));
                self.acc.extend_from_slice(&plaintext_chunk[4..]);
            }
            Some(_) => self.acc.extend_from_slice(plaintext_chunk),
        }

        let want = self.total_len.unwrap() as usize;
        if self.acc.len() >= want {
            self.acc.truncate(want);
            Ok(Some(std::mem::take(&mut self.acc)))
        } else {
            Ok(None)
        }
    }
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

fn params() -> Result<NoiseParams> {
    NOISE_PATTERN.parse().context("parse noise pattern string")
}

/// Turn-generic handshake loop: NK is exactly [initiator write, responder
/// write], but driving it via `is_my_turn()`/`is_handshake_finished()`
/// instead of hardcoding that order means this works unchanged if the
/// pattern ever changes.
fn run_handshake<S: Read + Write>(stream: &mut S, hs: &mut HandshakeState) -> Result<()> {
    while !hs.is_handshake_finished() {
        if hs.is_my_turn() {
            let mut buf = vec![0u8; NOISE_MSG_MAX];
            let n = hs.write_message(&[], &mut buf).context("noise handshake write")?;
            stream.write_all(&(n as u32).to_be_bytes())?;
            stream.write_all(&buf[..n])?;
        } else {
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).context("read handshake frame length")?;
            let clen = u32::from_be_bytes(len_buf) as usize;
            if clen == 0 || clen > NOISE_MSG_MAX {
                bail!("handshake ciphertext frame length {clen} out of range");
            }
            let mut cbuf = vec![0u8; clen];
            stream.read_exact(&mut cbuf).context("read handshake ciphertext")?;
            let mut pbuf = vec![0u8; clen];
            hs.read_message(&cbuf, &mut pbuf).context("noise handshake read")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny in-memory duplex "socket" so the handshake/transport logic can
    /// be tested without real sockets or threads: writes from one side land
    /// in a buffer the other side reads from, and vice versa.
    struct Duplex {
        write_to: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
        read_from_buf: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
        read_pos: usize,
    }
    impl Duplex {
        fn pair() -> (Duplex, Duplex) {
            let a_to_b = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let b_to_a = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            (
                Duplex { write_to: a_to_b.clone(), read_from_buf: b_to_a.clone(), read_pos: 0 },
                Duplex { write_to: b_to_a, read_from_buf: a_to_b, read_pos: 0 },
            )
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.write_to.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let avail = self.read_from_buf.borrow();
            let remaining = &avail[self.read_pos..];
            if remaining.is_empty() {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "no more data"));
            }
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.read_pos += n;
            Ok(n)
        }
    }

    #[test]
    fn hex_round_trips_including_zero_bytes() {
        let bytes = [0x00, 0xff, 0x01, 0xa2, 0x00];
        assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn hex_rejects_odd_length() {
        assert!(from_hex("abc").is_err());
    }

    #[test]
    fn handshake_and_round_trip_small_message() {
        let (priv_key, pub_key) = NoiseTransport::generate_keypair().unwrap();
        let (mut host_io, mut client_io) = Duplex::pair();

        // Handshake must interleave, so run each side on a thread-free
        // "take turns" basis isn't possible with blocking I/O over two
        // independent buffers -- drive it manually message-by-message
        // instead of calling handshake_initiator/responder directly, which
        // would deadlock both blocked on read(). This test exercises the
        // same run_handshake logic each of them calls internally.
        let params: NoiseParams = NOISE_PATTERN.parse().unwrap();
        let mut host_hs =
            Builder::new(params.clone()).local_private_key(&priv_key).unwrap().build_responder().unwrap();
        let mut client_hs =
            Builder::new(params).remote_public_key(&pub_key).unwrap().build_initiator().unwrap();

        // NK: message 1 initiator->responder, message 2 responder->initiator.
        let mut buf1 = vec![0u8; 65535];
        let n1 = client_hs.write_message(&[], &mut buf1).unwrap();
        let mut discard = vec![0u8; 65535];
        host_hs.read_message(&buf1[..n1], &mut discard).unwrap();

        let mut buf2 = vec![0u8; 65535];
        let n2 = host_hs.write_message(&[], &mut buf2).unwrap();
        client_hs.read_message(&buf2[..n2], &mut discard).unwrap();

        assert!(host_hs.is_handshake_finished());
        assert!(client_hs.is_handshake_finished());

        let mut host = NoiseTransport { state: host_hs.into_transport_mode().unwrap() };
        let mut client = NoiseTransport { state: client_hs.into_transport_mode().unwrap() };

        client.send(&mut client_io, b"hello from client").unwrap();
        let got = host.recv(&mut host_io).unwrap().unwrap();
        assert_eq!(got, b"hello from client");

        host.send(&mut host_io, b"hello from host").unwrap();
        let got = client.recv(&mut client_io).unwrap().unwrap();
        assert_eq!(got, b"hello from host");
    }

    #[test]
    fn chunks_large_payloads_and_reassembles() {
        let (priv_key, pub_key) = NoiseTransport::generate_keypair().unwrap();
        let (mut host_io, mut client_io) = Duplex::pair();

        let params: NoiseParams = NOISE_PATTERN.parse().unwrap();
        let mut host_hs =
            Builder::new(params.clone()).local_private_key(&priv_key).unwrap().build_responder().unwrap();
        let mut client_hs =
            Builder::new(params).remote_public_key(&pub_key).unwrap().build_initiator().unwrap();
        let mut buf1 = vec![0u8; 65535];
        let n1 = client_hs.write_message(&[], &mut buf1).unwrap();
        let mut discard = vec![0u8; 65535];
        host_hs.read_message(&buf1[..n1], &mut discard).unwrap();
        let mut buf2 = vec![0u8; 65535];
        let n2 = host_hs.write_message(&[], &mut buf2).unwrap();
        client_hs.read_message(&buf2[..n2], &mut discard).unwrap();

        let mut host = NoiseTransport { state: host_hs.into_transport_mode().unwrap() };
        let mut client = NoiseTransport { state: client_hs.into_transport_mode().unwrap() };

        // Bigger than CHUNK_MAX -- must span multiple wire frames, matching
        // what a large H.264 keyframe would need in the real pipeline.
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        client.send(&mut client_io, &big).unwrap();
        let got = host.recv(&mut host_io).unwrap().unwrap();
        assert_eq!(got, big);
    }
}
