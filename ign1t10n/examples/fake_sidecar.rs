//! A stand-in for `file-read-service`: binds `--bind host:port`, serves
//! HTTP 404 to anything under `/files/*` (the real sidecar returns 404
//! for unknown logical names, which is exactly what the health probe
//! reads as "alive"), and exits on SIGTERM. No fcntl, no files.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bind = args.windows(2).find(|w| w[0] == "--bind").map(|w| w[1].clone()).expect("--bind host:port");
    let config = args.windows(2).find(|w| w[0] == "--config").map(|w| w[1].clone()).unwrap_or_default();
    let l = TcpListener::bind(&bind).unwrap_or_else(|e| {
        eprintln!("bind {bind}: {e}");
        std::process::exit(2)
    });
    eprintln!("fake file-read-service bound {bind} (config={config})");
    for s in l.incoming().flatten() {
        std::thread::spawn(move || {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() {
                return;
            }
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
            }
            let body = r#"{"error":"unknown logical name"}"#;
            let mut w = s;
            let _ = write!(
                w,
                "HTTP/1.1 404 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        });
    }
}
