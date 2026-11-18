// SPDX-License-Identifier: GPL-3.0-or-later

//! `quorix`: command-line front end for Quorix Core.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Instant;

use clap::Parser;
use quorix_core::{download, Algorithm, CancelToken, DownloadRequest, Error, Event, Expected, Outcome, Target};

/// Download a file over HTTP or HTTPS.
///
/// The file is written to a temporary `.qrxpart` file and moved into place only
/// after its length, and its checksum when one is given, have been checked.
///
/// Press Ctrl+C once to pause: progress is saved and the same command continues
/// it. Press Ctrl+C twice to cancel and delete the partial file.
#[derive(Parser)]
#[command(name = "quorix", version)]
struct Args {
    /// Link to download (http or https)
    url: String,

    /// File or directory to save to. Defaults to the current directory.
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Use exactly this many connections (1 to 16). Without this option Quorix
    /// starts with 4 and adds more only while that makes the download faster.
    #[arg(short = 'c', long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=16))]
    connections: Option<u8>,

    /// Send from this local IP address. Repeat the option to use several network
    /// adapters at the same time, for example Ethernet and Wi-Fi.
    #[arg(long, value_name = "ADDRESS")]
    bind: Vec<std::net::IpAddr>,

    /// SHA-256 checksum the file must match
    #[arg(long, value_name = "HEX", conflicts_with = "blake3")]
    sha256: Option<String>,

    /// BLAKE3 checksum the file must match
    #[arg(long, value_name = "HEX")]
    blake3: Option<String>,

    /// Replace the destination file if it already exists
    #[arg(long)]
    overwrite: bool,

    /// Ignore a partial file from an earlier run and start from zero
    #[arg(long)]
    no_resume: bool,

    /// Reuse saved data without reading it back and comparing it with the saved hashes.
    /// Faster to continue a very large download. Not safe if another program changed the partial file.
    #[arg(long)]
    no_recheck: bool,

    /// Print nothing except errors
    #[arg(short, long)]
    quiet: bool,
}

const EXIT_FAILED: u8 = 1;
const EXIT_STOPPED: u8 = 130;

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match run(args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error[{}]: {e}", e.code());
            if e.is_retryable() {
                eprintln!("This kind of problem is often temporary. Running the same command again may work.");
            }
            ExitCode::from(if matches!(e, Error::Cancelled) { EXIT_STOPPED } else { EXIT_FAILED })
        }
    }
}

async fn run(args: Args) -> Result<ExitCode, Error> {
    let expected = match (&args.sha256, &args.blake3) {
        (Some(hex), _) => Some(Expected::parse(Algorithm::Sha256, hex)?),
        (_, Some(hex)) => Some(Expected::parse(Algorithm::Blake3, hex)?),
        _ => None,
    };

    let mut request = DownloadRequest::new(args.url, target_for(args.output));
    request.overwrite = args.overwrite;
    request.expected = expected;
    if let Some(fixed) = args.connections {
        request.connections = usize::from(fixed);
        request.adaptive = false;
    }
    request.local_addresses = args.bind;
    request.resume = !args.no_resume;
    request.verify_resume = !args.no_recheck;

    let cancel = CancelToken::new();
    let on_ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_ctrl_c.pause();
        }
        if tokio::signal::ctrl_c().await.is_ok() {
            on_ctrl_c.cancel();
        }
    });

    let progress = Progress::new(args.quiet);
    let result = download(&request, &cancel, &|event| progress.handle(event)).await;
    progress.finish();

    match result? {
        Outcome::Completed(done) => {
            if !args.quiet {
                println!("Saved {} ({})", done.path.display(), human(done.bytes));
                if done.resumed > 0 {
                    println!("Continued from an earlier run: {} were already on disk", human(done.resumed));
                }
                if let Some((algorithm, digest)) = &done.digest {
                    println!("{} verified: {digest}", algorithm.name());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Outcome::Paused { downloaded, total } => {
            eprintln!("Paused at {} of {}. Run the same command again to continue.", human(downloaded), human(total));
            Ok(ExitCode::from(EXIT_STOPPED))
        }
    }
}

/// A path that is an existing directory, or ends with a separator, means "put the file in here".
fn target_for(output: Option<PathBuf>) -> Target {
    match output {
        None => Target::Directory(PathBuf::from(".")),
        Some(path) => {
            let text = path.to_string_lossy();
            if path.is_dir() || text.ends_with('/') || text.ends_with('\\') {
                Target::Directory(path)
            } else {
                Target::File(path)
            }
        }
    }
}

/// Draws one progress line on stderr when it is a terminal.
struct Progress {
    /// Draw the live progress line. Only when stderr is a terminal.
    enabled: bool,
    quiet: bool,
    inner: Mutex<Meter>,
}

struct Meter {
    started: Instant,
    /// Bytes that were on disk before this run; they are not part of this run's speed.
    resumed: u64,
    connections: usize,
    last_draw: Option<Instant>,
}

impl Progress {
    fn new(quiet: bool) -> Self {
        let meter = Meter { started: Instant::now(), resumed: 0, connections: 1, last_draw: None };
        Self { enabled: !quiet && std::io::stderr().is_terminal(), quiet, inner: Mutex::new(meter) }
    }

    fn handle(&self, event: Event) {
        // A result of the check is a fact the user must see, also when the output goes to a file.
        if let Event::PathDown(address) = event {
            if !self.quiet {
                eprintln!("The network adapter with the address {address} cannot reach the server. Quorix continues without it.");
            }
            return;
        }
        if let Event::Rechecked { verified, discarded } = event {
            if discarded > 0 && !self.quiet {
                eprintln!(
                    "Checked the saved data: {} are good, {} did not pass the check and will be downloaded again.",
                    human(verified),
                    human(discarded)
                );
            }
            return;
        }
        if !self.enabled {
            return;
        }
        let mut meter = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match event {
            Event::Started { resumed, connections, .. } => {
                meter.started = Instant::now();
                meter.resumed = resumed;
                meter.connections = connections;
            }
            Event::Progress { downloaded, total } => {
                let finished = total == Some(downloaded);
                if !finished && meter.last_draw.is_some_and(|t| t.elapsed().as_millis() < 250) {
                    return;
                }
                meter.last_draw = Some(Instant::now());

                let seconds = meter.started.elapsed().as_secs_f64().max(0.001);
                let speed = human((downloaded.saturating_sub(meter.resumed) as f64 / seconds) as u64);
                let line = match total {
                    Some(total) if total > 0 => format!(
                        "{:>3}%  {} of {}  {speed}/s  {} connection{}",
                        downloaded * 100 / total,
                        human(downloaded),
                        human(total),
                        meter.connections,
                        if meter.connections == 1 { "" } else { "s" }
                    ),
                    _ => format!("{}  {speed}/s", human(downloaded)),
                };
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "\r{line:<70}");
                let _ = err.flush();
            }
            Event::Connections(connections) => meter.connections = connections,
            Event::Rechecked { .. } | Event::PathDown(_) => {}
            Event::State(_) => {}
        }
    }

    fn finish(&self) {
        if self.enabled && self.inner.lock().unwrap_or_else(|p| p.into_inner()).last_draw.is_some() {
            eprintln!();
        }
    }
}

/// Byte count in binary units, one decimal place above bytes.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.0 KiB");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(6_549_825_126), "6.1 GiB");
    }

    #[test]
    fn output_ending_in_a_separator_is_a_directory() {
        assert!(matches!(target_for(None), Target::Directory(_)));
        assert!(matches!(target_for(Some("downloads/".into())), Target::Directory(_)));
        assert!(matches!(target_for(Some("file.zip".into())), Target::File(_)));
    }

    #[test]
    fn connection_count_is_limited_to_one_through_sixteen() {
        assert!(Args::try_parse_from(["quorix", "https://example.com/f", "-c", "16"]).is_ok());
        assert!(Args::try_parse_from(["quorix", "https://example.com/f", "-c", "0"]).is_err());
        assert!(Args::try_parse_from(["quorix", "https://example.com/f", "-c", "17"]).is_err());
    }
}
