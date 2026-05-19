//! `audiophore-cli`: binary entry point.
//!
//! Hosts the top-level `audiophore` binary and its `clap`-derived
//! subcommand router. M1 ships the [`monitor`](Command::Monitor)
//! subcommand: a thin wrapper around [`audiophore_audio::dispatch_packet`]
//! that listens on a UDP port, decodes Synesthesia OSC, and pretty-prints
//! each resulting [`audiophore_core::AudioFrame`].
//!
//! See `implementation.md` §1.5 and `planning/SYNESTHESIA_OSC.md`
//! *Sanity-check tooling to write early* for the design context.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use audiophore_audio::{AudioFrameBuilder, RECV_BUFFER_BYTES, dispatch_packet};
use audiophore_core::AudioFrame;
use clap::{Parser, Subcommand};
use rosc::{OscPacket, OscType};
use tokio::net::UdpSocket;

/// Top-level CLI parser.
#[derive(Debug, Parser)]
#[command(name = "audiophore", version, about = "Audiophore CLI")]
struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// Subcommands exposed by the `audiophore` binary.
#[derive(Debug, Subcommand)]
enum Command {
    /// Listen on UDP for Synesthesia OSC and pretty-print each frame.
    Monitor(MonitorArgs),
}

/// Arguments for `audiophore monitor`.
#[derive(Debug, clap::Args)]
struct MonitorArgs {
    /// UDP port to listen on.
    #[arg(long, default_value_t = 9000)]
    port: u16,

    /// Address to bind. Defaults to all interfaces (`0.0.0.0`).
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::UNSPECIFIED))]
    bind: IpAddr,

    /// Dump every incoming OSC message's raw address and argument shape
    /// in addition to the frame line. Use to build the wire-dump
    /// artifact consumed by the workspace-level B.2 schema-lock PR.
    #[arg(long)]
    raw: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Command::Monitor(args) => run_monitor(args).await,
    }
}

/// Install a tracing subscriber that honors `RUST_LOG` (default `info`)
/// so `tracing` events emitted by `audiophore-audio` reach the user.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // If a global subscriber is already set we silently swallow the
    // error: the binary still runs, just without our preferred filter.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// Run the `monitor` subcommand: bind a UDP socket, decode each
/// datagram with [`dispatch_packet`], print one frame per line plus an
/// optional raw OSC dump.
async fn run_monitor(args: MonitorArgs) -> Result<()> {
    let addr = SocketAddr::new(args.bind, args.port);
    let socket = UdpSocket::bind(addr)
        .await
        .with_context(|| format!("binding UDP socket on {addr}"))?;
    let bound = socket
        .local_addr()
        .context("querying local UDP socket address")?;
    tracing::info!(addr = %bound, raw = args.raw, "audiophore monitor listening");
    println!("# audiophore monitor — bound to {bound}");
    println!(
        "# columns: t\\tbpm\\tbeat_phase\\ton_beat\\tlevels.bass\\tlevels.mid\\tlevels.high\\tintensity\\tfade"
    );

    let mut builder = AudioFrameBuilder::new();
    let mut buf = vec![0_u8; RECV_BUFFER_BYTES];
    let mut shutdown = std::pin::pin!(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            biased;
            res = shutdown.as_mut() => {
                res.context("installing ctrl-c handler")?;
                tracing::info!("shutdown requested; exiting");
                break;
            }
            recv = socket.recv_from(&mut buf) => {
                let (n, peer) = recv.context("receiving UDP datagram")?;
                let bytes = &buf[..n];
                if args.raw {
                    dump_raw(bytes, peer);
                }
                if let Err(e) = dispatch_packet(bytes, &mut builder) {
                    tracing::warn!(error = %e, %peer, "skipped malformed OSC datagram");
                    continue;
                }
                let frame = builder.snapshot();
                println!("{}", format_frame(&frame));
            }
        }
    }
    Ok(())
}

/// Format an [`AudioFrame`] as a single tab-separated line in the
/// documented column order.
///
/// Column order (one frame per line, tab-separated):
/// `t  bpm  beat_phase  on_beat  levels.bass  levels.mid  levels.high  intensity  fade`.
fn format_frame(f: &AudioFrame) -> String {
    format!(
        "{t:.4}\t{bpm:.2}\t{phase:.4}\t{beat}\t{bass:.4}\t{mid:.4}\t{high:.4}\t{intensity:.4}\t{fade:.4}",
        t = f.t,
        bpm = f.bpm,
        phase = f.beat_phase,
        beat = u8::from(f.on_beat),
        bass = f.levels.bass,
        mid = f.levels.mid,
        high = f.levels.high,
        intensity = f.intensity,
        fade = f.fade,
    )
}

/// Decode `bytes` for inspection only and print every contained OSC
/// message's address + argument shape, one per line.
///
/// Best-effort: malformed datagrams are surfaced as a single warning
/// line so the dispatch path can still log its own error context.
fn dump_raw(bytes: &[u8], peer: SocketAddr) {
    match rosc::decoder::decode_udp(bytes) {
        Ok((_, packet)) => walk_for_raw(&packet, peer),
        Err(e) => {
            tracing::warn!(error = ?e, %peer, "raw: failed to decode OSC for dump");
        }
    }
}

fn walk_for_raw(packet: &OscPacket, peer: SocketAddr) {
    match packet {
        OscPacket::Message(msg) => {
            println!("raw\t{peer}\t{}\t{}", msg.addr, fmt_args(&msg.args));
        }
        OscPacket::Bundle(bundle) => {
            for inner in &bundle.content {
                walk_for_raw(inner, peer);
            }
        }
    }
}

/// Render an OSC argument vector as a compact `tag:value` list (e.g.
/// `f:0.42 i:1`). Truncates `String`/`Blob` payloads so a chatty sender
/// can't blow up the line.
fn fmt_args(args: &[OscType]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // `write!` into a `String` is infallible; the `_` discards the
        // `Ok(())` while keeping clippy::format_push_string happy.
        match arg {
            OscType::Float(v) => {
                let _ = write!(out, "f:{v}");
            }
            OscType::Double(v) => {
                let _ = write!(out, "d:{v}");
            }
            OscType::Int(v) => {
                let _ = write!(out, "i:{v}");
            }
            OscType::Long(v) => {
                let _ = write!(out, "h:{v}");
            }
            OscType::Bool(v) => {
                let _ = write!(out, "b:{v}");
            }
            OscType::String(s) => {
                let _ = write!(out, "s:{}", truncate_str(s, 32));
            }
            OscType::Char(c) => {
                let _ = write!(out, "c:{c}");
            }
            OscType::Color(c) => {
                let _ = write!(
                    out,
                    "r:{:02x}{:02x}{:02x}{:02x}",
                    c.red, c.green, c.blue, c.alpha
                );
            }
            OscType::Blob(b) => {
                let _ = write!(out, "blob[{}]", b.len());
            }
            OscType::Time(_) => out.push_str("t:<timetag>"),
            OscType::Midi(_) => out.push_str("m:<midi>"),
            OscType::Array(a) => {
                let _ = write!(out, "a[{}]", a.content.len());
            }
            OscType::Nil => out.push('N'),
            OscType::Inf => out.push('I'),
        }
    }
    out
}

fn truncate_str(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let head: String = s.chars().take(n).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests panic on assertion failure by design"
)]
mod tests {
    use super::{Cli, fmt_args, format_frame, truncate_str};
    use audiophore_core::{AudioFrame, BandValues};
    use clap::Parser;
    use rosc::OscType;

    fn sample_frame() -> AudioFrame {
        AudioFrame {
            t: 12.345_678,
            bpm: 128.5,
            bpm_confidence: 0.9,
            beat_phase: 0.5,
            on_beat: true,
            levels: BandValues {
                whole: 0.7,
                bass: 0.75,
                mid: 0.4,
                mid_high: 0.3,
                high: 0.2,
            },
            hits: BandValues::default(),
            presence: BandValues::default(),
            intensity: 0.42,
            fade: 0.88,
        }
    }

    #[test]
    fn format_frame_emits_nine_tab_separated_columns_in_documented_order() {
        let line = format_frame(&sample_frame());
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(cols.len(), 9, "expected 9 cols, got {cols:?}");
        // Spot-check the ones that drive downstream wire-dump parsing.
        assert!(cols[0].starts_with("12.345"), "t column: {}", cols[0]);
        assert_eq!(cols[1], "128.50", "bpm column");
        assert_eq!(cols[3], "1", "on_beat column should be 0|1");
        assert_eq!(cols[4], "0.7500", "levels.bass column");
        assert_eq!(cols[7], "0.4200", "intensity column");
        assert_eq!(cols[8], "0.8800", "fade column");
    }

    #[test]
    fn format_frame_on_beat_false_renders_zero() {
        let mut f = sample_frame();
        f.on_beat = false;
        let cols: Vec<String> = format_frame(&f).split('\t').map(str::to_owned).collect();
        assert_eq!(cols[3], "0");
    }

    #[test]
    fn fmt_args_renders_known_types() {
        let s = fmt_args(&[OscType::Float(0.42), OscType::Int(1), OscType::Bool(true)]);
        assert_eq!(s, "f:0.42 i:1 b:true");
    }

    #[test]
    fn fmt_args_truncates_long_strings() {
        let long = "x".repeat(64);
        let s = fmt_args(&[OscType::String(long)]);
        assert!(s.starts_with("s:"));
        // 32 chars + ellipsis fits well under the original 64.
        assert!(
            s.chars().count() < 40,
            "expected truncation, got len {}",
            s.chars().count()
        );
    }

    #[test]
    fn truncate_str_preserves_short_strings() {
        assert_eq!(truncate_str("hi", 32), "hi");
    }

    #[test]
    fn cli_parses_monitor_with_defaults() {
        let cli = Cli::try_parse_from(["audiophore", "monitor"]).expect("parse");
        match cli.command {
            super::Command::Monitor(args) => {
                assert_eq!(args.port, 9000);
                assert!(!args.raw);
            }
        }
    }

    #[test]
    fn cli_parses_monitor_with_port_and_raw() {
        let cli = Cli::try_parse_from(["audiophore", "monitor", "--port", "9001", "--raw"])
            .expect("parse");
        match cli.command {
            super::Command::Monitor(args) => {
                assert_eq!(args.port, 9001);
                assert!(args.raw);
            }
        }
    }
}
