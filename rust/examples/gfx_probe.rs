//! Probe: how fast can a terminal take a live pixel chart?
//!
//! `cargo run --release --example gfx_probe -- <iterm|kitty> <frames> <out.txt>`
//! Fills the whole terminal with a palette PNG of candles (one candle
//! changes per frame, like a live chart), sends it with the iTerm2 inline
//! image protocol or the kitty graphics protocol, and writes timings to
//! `out.txt`.  Run it inside the terminal under test.

use std::io::Write;
use std::time::Instant;

use dos::agent::chart_png::{base64, png};

const PALETTE: [[u8; 3]; 4] = [
    [0x00, 0x00, 0xa8],
    [0x30, 0x30, 0xc8],
    [0x55, 0xff, 0x55],
    [0xff, 0x55, 0x55],
];

fn draw(w: usize, h: usize, frame: usize) -> Vec<u8> {
    let mut px = vec![0u8; w * h];
    for y in (0..h).step_by(h / 8) {
        px[y * w..y * w + w].fill(1);
    }
    let n = 200;
    let cw = w / n;
    let mut p = 0.5f64;
    for i in 0..n {
        let wave =
            ((i + frame) as f64 * 0.17).sin() * 0.02 + ((i * 7 + 3) % 11) as f64 * 0.002 - 0.01;
        let open = p;
        p = (p + wave).clamp(0.05, 0.95);
        let close = p;
        let (lo, hi) = (open.min(close) - 0.01, open.max(close) + 0.01);
        let c = if close >= open { 2 } else { 3 };
        let y = |v: f64| ((1.0 - v) * (h as f64 - 1.0)) as usize;
        let x0 = i * cw;
        let mid = x0 + cw / 2;
        for yy in y(hi)..=y(lo) {
            px[yy * w + mid] = c;
        }
        for yy in y(open.max(close))..=y(open.min(close)) {
            for xx in x0 + 1..x0 + cw.saturating_sub(1) {
                px[yy * w + xx] = c;
            }
        }
    }
    px
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let proto = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("iterm")
        .to_string();
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let out = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "/tmp/gfx_probe.txt".into());
    let ws = crossterm::terminal::window_size().expect("window size");
    let (w, h) = (ws.width as usize, ws.height as usize);
    let (cols, rows) = (ws.columns, ws.rows);
    let mut so = std::io::stdout().lock();
    let mut log = format!("proto={proto} px={w}x{h} cells={cols}x{rows}\n");
    let (mut t_draw, mut t_png, mut t_send, mut bytes) = (0f64, 0f64, 0f64, 0usize);
    let start = Instant::now();
    for f in 0..frames {
        let t0 = Instant::now();
        let px = draw(w, h, f);
        let t1 = Instant::now();
        let data = png(w, h, &PALETTE, &px);
        let b64 = base64(&data);
        let t2 = Instant::now();
        let seq = if proto == "kitty" {
            // Transmit + display, image id 1 replaced each frame, chunks of 4096.
            let mut s = String::from("\x1b[H");
            let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
            for (i, c) in chunks.iter().enumerate() {
                let more = if i + 1 < chunks.len() { 1 } else { 0 };
                if i == 0 {
                    s.push_str(&format!(
                        "\x1b_Ga=T,f=100,i=1,q=2,C=1,c={cols},r={rows},m={more};"
                    ));
                } else {
                    s.push_str(&format!("\x1b_Gm={more};"));
                }
                s.push_str(std::str::from_utf8(c).unwrap());
                s.push_str("\x1b\\");
            }
            s
        } else {
            format!(
                "\x1b[H\x1b]1337;File=inline=1;size={};width={cols};height={rows};preserveAspectRatio=0:{b64}\x07",
                data.len()
            )
        };
        so.write_all(seq.as_bytes()).unwrap();
        so.flush().unwrap();
        let t3 = Instant::now();
        t_draw += (t1 - t0).as_secs_f64();
        t_png += (t2 - t1).as_secs_f64();
        t_send += (t3 - t2).as_secs_f64();
        bytes += seq.len();
    }
    let total = start.elapsed().as_secs_f64();
    let n = frames as f64;
    log.push_str(&format!(
        "frames={frames} total={total:.3}s fps={:.1}\nper frame: draw {:.2} ms, png+b64 {:.2} ms, write {:.2} ms, {:.0} KB\n",
        n / total,
        t_draw / n * 1e3,
        t_png / n * 1e3,
        t_send / n * 1e3,
        bytes as f64 / n / 1024.0
    ));
    std::fs::write(&out, log).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(4));
}
