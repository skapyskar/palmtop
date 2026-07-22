//! Phase 0 spike: serve Annex-B H.264 access units over TCP to the Android
//! decode spike, paced at a target framerate.
//!
//! Wire format is deliberately the simplest thing that works:
//!   [4-byte big-endian length][access unit bytes (Annex-B, with start codes)]
//!
//! Run:  cargo run -p spike-h264-server -- android-spike/test-1080p.h264 [fps] [port]

use std::io::Write;
use std::net::TcpListener;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

fn main() -> Result<()> {
    // Port/framerate default to the local host profile; args override for A/B runs.
    let cfg = palmtop_config::HostConfig::load()?;

    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "android-spike/test-1080p.h264".into());
    let fps: f64 = match args.next() {
        Some(v) => v.parse()?,
        None => cfg.encode.fps as f64,
    };
    let port: u16 = match args.next() {
        Some(v) => v.parse()?,
        None => cfg.host.port,
    };
    println!("[cfg] host {}:{port}", cfg.resolved_ip()?);

    let data = std::fs::read(&path).with_context(|| format!("read {path}"))?;
    let aus = split_access_units(&data);
    println!(
        "[ok] loaded {} ({} bytes) -> {} access units",
        path,
        data.len(),
        aus.len()
    );
    if aus.is_empty() {
        anyhow::bail!("no access units found -- is this an Annex-B H.264 stream?");
    }

    let listener = TcpListener::bind(("0.0.0.0", port))?;
    println!("[..] listening on 0.0.0.0:{port}, pacing at {fps} fps -- waiting for the phone");

    for stream in listener.incoming() {
        let mut sock = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[warn] accept failed: {e}");
                continue;
            }
        };
        let peer = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        println!("[ok] client connected: {peer}");
        sock.set_nodelay(true).ok(); // latency over throughput

        let frame_interval = Duration::from_secs_f64(1.0 / fps);
        let start = Instant::now();
        let mut sent: u64 = 0;

        'session: loop {
            for au in &aus {
                // Pace against a fixed schedule so we don't drift.
                let due = start + frame_interval * (sent as u32);
                let now = Instant::now();
                if due > now {
                    std::thread::sleep(due - now);
                }

                let len = (au.len() as u32).to_be_bytes();
                if sock.write_all(&len).is_err() || sock.write_all(au).is_err() {
                    println!("[ok] client disconnected after {sent} frames");
                    break 'session;
                }
                sent += 1;
                if sent.is_multiple_of(150) {
                    println!(
                        "[..] sent {sent} frames ({:.1} fps actual)",
                        sent as f64 / start.elapsed().as_secs_f64()
                    );
                }
            }
        }
    }
    Ok(())
}

/// Split an Annex-B stream into access units.
///
/// Groups each VCL NAL (types 1 and 5) together with any parameter-set/SEI NALs
/// that precede it, so every emitted unit is independently decodable in order.
fn split_access_units(data: &[u8]) -> Vec<Vec<u8>> {
    let starts = find_start_codes(data);
    let mut aus: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();

    for (i, &(pos, sc_len)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map(|&(p, _)| p).unwrap_or(data.len());
        let nal = &data[pos..end];
        let nal_type = data
            .get(pos + sc_len)
            .map(|b| b & 0x1f)
            .unwrap_or(0);

        current.extend_from_slice(nal);

        // NAL types 1 (non-IDR slice) and 5 (IDR slice) terminate an access unit.
        if nal_type == 1 || nal_type == 5 {
            aus.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        aus.push(current);
    }
    aus
}

/// Returns (offset_of_start_code, start_code_length) for each NAL in the stream.
fn find_start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                out.push((i, 3));
                i += 3;
                continue;
            } else if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                out.push((i, 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    out
}
