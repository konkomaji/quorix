// SPDX-License-Identifier: GPL-3.0-or-later

//! Quorix Bench: repeatable speed and integrity measurements.
//!
//! Every scenario runs against a server on this computer, so the network is
//! not a variable. The numbers show how the engine behaves under a known
//! server condition. They are not internet speeds and must not be quoted as such.
//!
//! Each run downloads to a new empty folder, and the result is compared with
//! the expected content. A run that produces a wrong file counts as an
//! integrity failure, whatever its speed.

use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clap::Parser;
use quorix_core::{download, CancelToken, DownloadRequest, Event, Outcome, Target, TaskState};
use quorix_integrity::{Algorithm, Hasher};
use quorix_testserver::{body, TestServer};

const MIB: usize = 1024 * 1024;

/// Measure Quorix against a local server under controlled conditions.
#[derive(Parser)]
#[command(name = "quorix-bench", version)]
struct Args {
    /// Runs for each row. The table shows the median, the minimum and the maximum.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=50))]
    runs: u8,

    /// Use small files. For a fast check that the tool operates, not for results.
    #[arg(long)]
    quick: bool,

    /// Do not run other tools (curl, aria2c) for comparison.
    #[arg(long)]
    no_compare: bool,
}

struct Scenario {
    name: &'static str,
    /// What the scenario shows, in one sentence, for the report.
    shows: &'static str,
    route: &'static str,
    size: usize,
    connections: &'static [usize],
}

fn scenarios(quick: bool) -> Vec<Scenario> {
    let scale = |full: usize, small: usize| if quick { small } else { full };
    vec![
        Scenario {
            name: "Server limits each connection",
            shows: "The server sends slowly on each connection. More connections must give more speed.",
            route: "range-slow",
            size: scale(6 * MIB, MIB),
            connections: &[1, 4, 8, 16],
        },
        Scenario {
            name: "No server limit",
            shows: "The server sends as fast as it can. This is the ceiling of the engine and the disk on this computer.",
            route: "range",
            size: scale(256 * MIB, 32 * MIB),
            connections: &[1, 4, 8],
        },
        Scenario {
            name: "Server without range support",
            shows: "The server ignores range requests. The engine must use one request.",
            route: "ok",
            size: scale(128 * MIB, 16 * MIB),
            connections: &[4],
        },
    ]
}

#[derive(Clone, Copy)]
enum Tool {
    Quorix,
    Curl,
    Aria2,
}

impl Tool {
    fn label(self) -> &'static str {
        match self {
            Tool::Quorix => "Quorix",
            Tool::Curl => "curl",
            Tool::Aria2 => "aria2c",
        }
    }

    /// True when the tool can be started on this computer.
    fn available(self) -> bool {
        let program = match self {
            Tool::Quorix => return true,
            Tool::Curl => "curl",
            Tool::Aria2 => "aria2c",
        };
        Command::new(program).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }
}

struct Row {
    scenario: &'static str,
    tool: &'static str,
    connections: usize,
    speeds: Vec<f64>,
    failed_runs: usize,
    wrong_files: usize,
    /// Milliseconds spent on the final sync and move, for tools that report it.
    commits: Vec<f64>,
}

fn blake3_of_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Hasher::new(Algorithm::Blake3);
    let mut buffer = vec![0u8; MIB];
    loop {
        match file.read(&mut buffer)? {
            0 => return Ok(hasher.finalize_hex()),
            n => hasher.update(&buffer[..n]),
        }
    }
}

/// Runs one download with one tool. Returns the seconds it took, or `None` when the tool reported a failure.
/// The second value is the time of the final sync and move, when the tool makes it visible.
async fn run_once(tool: Tool, url: &str, dest: &Path, connections: usize) -> Option<(Duration, Option<Duration>)> {
    let mut commit = None;
    let started = Instant::now();
    let ok = match tool {
        Tool::Quorix => {
            let mut request = DownloadRequest::new(url, Target::File(dest.to_path_buf()));
            request.connections = connections;
            request.http.use_system_proxy = false;
            let finalizing_since = std::sync::Mutex::new(None);
            let outcome = download(&request, &CancelToken::new(), &|event| {
                if matches!(event, Event::State(TaskState::Finalizing)) {
                    *finalizing_since.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
                }
            })
            .await;
            commit = finalizing_since.into_inner().ok().flatten().map(|since: Instant| since.elapsed());
            matches!(outcome, Ok(Outcome::Completed(_)))
        }
        Tool::Curl => {
            let (url, dest) = (url.to_string(), dest.to_path_buf());
            tokio::task::spawn_blocking(move || {
                Command::new("curl")
                    .args(["--silent", "--fail", "--noproxy", "*", "--output"])
                    .arg(&dest)
                    .arg(&url)
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .await
            .unwrap_or(false)
        }
        Tool::Aria2 => {
            let (url, dir, name) = (url.to_string(), dest.parent()?.to_path_buf(), dest.file_name()?.to_os_string());
            let n = connections.min(16).to_string();
            tokio::task::spawn_blocking(move || {
                Command::new("aria2c")
                    .args([
                        "--quiet",
                        "--no-conf",
                        "--file-allocation=none",
                        "--min-split-size=1M",
                        "--all-proxy=",
                        "-x",
                        &n,
                        "-s",
                        &n,
                        "-d",
                    ])
                    .arg(&dir)
                    .arg("-o")
                    .arg(&name)
                    .arg(&url)
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .await
            .unwrap_or(false)
        }
    };
    ok.then(|| (started.elapsed(), commit))
}

async fn measure(scenario: &Scenario, tool: Tool, connections: usize, runs: usize, expected: &str) -> Row {
    let mut row = Row {
        scenario: scenario.name,
        tool: tool.label(),
        connections,
        speeds: Vec::new(),
        failed_runs: 0,
        wrong_files: 0,
        commits: Vec::new(),
    };
    for _ in 0..runs {
        // A new server and a new folder for each run: no warm connections, no saved progress, no reused file.
        let server = TestServer::start().await;
        let Ok(dir) = tempfile::tempdir() else {
            row.failed_runs += 1;
            continue;
        };
        let dest = dir.path().join("file.bin");
        let url = server.url(&format!("/{}/{}", scenario.route, scenario.size));

        match run_once(tool, &url, &dest, connections).await {
            None => row.failed_runs += 1,
            Some((elapsed, commit)) => {
                let path = dest.clone();
                let actual = tokio::task::spawn_blocking(move || blake3_of_file(&path)).await.ok().and_then(Result::ok);
                if actual.as_deref() == Some(expected) {
                    row.speeds.push(scenario.size as f64 / MIB as f64 / elapsed.as_secs_f64().max(1e-9));
                    row.commits.extend(commit.map(|c| c.as_secs_f64() * 1000.0));
                } else {
                    row.wrong_files += 1;
                }
            }
        }
    }
    row.speeds.sort_by(f64::total_cmp);
    row.commits.sort_by(f64::total_cmp);
    row
}

fn median(sorted: &[f64]) -> Option<f64> {
    match sorted.len() {
        0 => None,
        n if n % 2 == 1 => Some(sorted[n / 2]),
        n => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0),
    }
}

fn speed(value: Option<f64>) -> String {
    value.map_or("-".to_string(), |v| if v >= 100.0 { format!("{v:.0}") } else { format!("{v:.1}") })
}

fn print_report(args: &Args, scenarios: &[Scenario], rows: &[Row], tools: &[Tool]) {
    println!("# Quorix Bench report\n");
    println!("- Quorix version: {}", env!("CARGO_PKG_VERSION"));
    println!("- Build: {}", if cfg!(debug_assertions) { "debug (do not use these numbers; build with --release)" } else { "release" });
    println!(
        "- System: {} {}, {} logical processors",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism().map_or(0, usize::from)
    );
    println!("- Runs for each row: {}", args.runs);
    println!("- Tools: {}", tools.iter().map(|t| t.label()).collect::<Vec<_>>().join(", "));
    println!("- File sizes: {}", if args.quick { "small (quick mode, not for results)" } else { "full" });
    println!();
    println!("The server runs on this computer. These numbers show the behaviour of each tool under a known server condition.");
    println!("They are not internet speeds.\n");
    println!("How to read the table:\n");
    println!("- The time of a Quorix run includes the sync of the file to the disk and the move to the destination.");
    println!("  Quorix does not report a file as complete before that. The column \"Commit ms\" shows this part.");
    println!("- curl and aria2c do not sync the file. Their time ends when the data is given to the operating system.");
    println!("- Thus a difference in MiB/s on a fast local server is partly a difference in what \"complete\" means.");
    println!(
        "- The rows run one after the other on one disk. A row can be slower because the disk is busy with data from the row before it.\n"
    );

    for scenario in scenarios {
        println!("## {}\n", scenario.name);
        println!("{} File size: {} MiB.\n", scenario.shows, scenario.size / MIB);
        println!("| Tool | Connections | Median MiB/s | Minimum | Maximum | Commit ms | Good runs | Failed runs | Wrong files |");
        println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|");
        for row in rows.iter().filter(|r| r.scenario == scenario.name) {
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                row.tool,
                row.connections,
                speed(median(&row.speeds)),
                speed(row.speeds.first().copied()),
                speed(row.speeds.last().copied()),
                median(&row.commits).map_or("-".to_string(), |ms| format!("{ms:.0}")),
                row.speeds.len(),
                row.failed_runs,
                row.wrong_files
            );
        }
        println!();
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let scenarios = scenarios(args.quick);
    let mut tools = vec![Tool::Quorix];
    if !args.no_compare {
        tools.extend([Tool::Curl, Tool::Aria2].into_iter().filter(|t| t.available()));
    }

    let mut rows = Vec::new();
    for scenario in &scenarios {
        let expected = {
            let mut hasher = Hasher::new(Algorithm::Blake3);
            hasher.update(&body(scenario.size));
            hasher.finalize_hex()
        };
        for &tool in &tools {
            // curl uses one connection, so it has one row for each scenario.
            let counts: &[usize] = if matches!(tool, Tool::Curl) { &[1] } else { scenario.connections };
            for &connections in counts {
                eprintln!("{}: {} with {} connection(s)", scenario.name, tool.label(), connections);
                rows.push(measure(scenario, tool, connections, usize::from(args.runs), &expected).await);
            }
        }
    }

    print_report(&args, &scenarios, &rows, &tools);
    // A wrong file from Quorix is a release-blocking defect. Make it impossible to miss in automation.
    let quorix_wrong: usize = rows.iter().filter(|r| r.tool == Tool::Quorix.label()).map(|r| r.wrong_files + r.failed_runs).sum();
    if quorix_wrong > 0 {
        eprintln!("Quorix produced {quorix_wrong} failed or wrong result(s).");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_sorted_values() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0]), Some(3.0));
        assert_eq!(median(&[1.0, 3.0]), Some(2.0));
        assert_eq!(median(&[1.0, 2.0, 9.0]), Some(2.0));
    }

    #[test]
    fn speeds_are_formatted_for_a_table() {
        assert_eq!(speed(None), "-");
        assert_eq!(speed(Some(3.26)), "3.3");
        assert_eq!(speed(Some(1234.5)), "1234");
    }

    #[tokio::test]
    async fn a_quorix_row_is_measured_and_verified() {
        let scenario = Scenario { name: "t", shows: "", route: "range", size: 300_000, connections: &[2] };
        let mut hasher = Hasher::new(Algorithm::Blake3);
        hasher.update(&body(scenario.size));
        let row = measure(&scenario, Tool::Quorix, 2, 2, &hasher.finalize_hex()).await;
        assert_eq!((row.speeds.len(), row.failed_runs, row.wrong_files), (2, 0, 0));
    }

    #[tokio::test]
    async fn a_file_that_does_not_match_is_counted_as_wrong_not_as_a_speed() {
        let scenario = Scenario { name: "t", shows: "", route: "range", size: 300_000, connections: &[1] };
        let row = measure(&scenario, Tool::Quorix, 1, 1, "not the real digest").await;
        assert_eq!((row.speeds.len(), row.wrong_files), (0, 1));
    }
}
