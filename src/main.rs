//! run-hidden-rs — a Rust port of `run-hidden`.
//!
//! Runs a program with its console window hidden (on Windows). By default the
//! child's stdout/stderr are forwarded to our own; with `--stdout-path` /
//! `--stderr-path` they are redirected to files instead, and `--stdin-path`
//! feeds the child's stdin from a file. The child is killed if we are asked to
//! terminate (Ctrl-C, SIGTERM, console close, ...).
//!
//! Command line shape:
//!
//! ```text
//! run-hidden-rs [OPTIONS] -- <program> [args...]
//! ```
//!
//! Everything before `--` is parsed by us; everything after `--` is the program
//! to run and is forwarded to it verbatim — we never join the arguments into a
//! single string and split on spaces, so arguments containing spaces, quotes or
//! anything else survive untouched.

// Build as a GUI-subsystem app on Windows (like the original's `wWinMain`). A
// console-subsystem exe makes Windows *create* a console window whenever it is
// launched without one (Explorer, Task Scheduler, a shortcut) — that's the
// "black box". The GUI subsystem never gets a console, so no window ever flashes.
#![windows_subsystem = "windows"]

use std::env;
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;

use clap::Parser;
use shared_child::SharedChild;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW` from the Win32 API: the child runs without a console
/// window, which is the whole point of `run-hidden`.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Parser)]
#[command(
    name = "run-hidden-rs",
    version,
    about = "Run a program with its console window hidden, forwarding or redirecting its stdio.",
    override_usage = "run-hidden-rs [OPTIONS] -- <program> [args...]",
    after_help = "Everything after `--` is the program to run and its arguments, forwarded verbatim."
)]
struct Cli {
    /// Feed the child's stdin from this file (default: the null device).
    #[arg(long, value_name = "FILE")]
    stdin_path: Option<PathBuf>,

    /// Write the child's stdout to this file (default: forward to our stdout).
    #[arg(long, value_name = "FILE")]
    stdout_path: Option<PathBuf>,

    /// Write the child's stderr to this file (default: forward to our stderr).
    #[arg(long, value_name = "FILE")]
    stderr_path: Option<PathBuf>,
}

fn main() {
    // As a GUI-subsystem app we have no console of our own. If we *were* launched
    // from one (e.g. cmd.exe), reattach to it so the output we forward is visible.
    // This only ever attaches to an existing parent console — it never creates a
    // new window — so the "no black box" guarantee holds either way.
    #[cfg(windows)]
    attach_parent_console();

    // Split argv at the first `--`: the part before it is ours to parse, the
    // part after it is the command to launch (forwarded verbatim). clap never
    // sees the child's arguments, so the child's own flags can't collide with
    // ours.
    let argv: Vec<OsString> = env::args_os().collect();
    let (clap_args, command): (&[OsString], &[OsString]) =
        match argv.iter().position(|a| a == "--") {
            Some(i) => (&argv[..i], &argv[i + 1..]),
            None => (&argv[..], &[][..]),
        };

    // clap handles --help/--version/parse errors by exiting on its own.
    let cli = Cli::parse_from(clap_args);

    std::process::exit(run(cli, command));
}

fn run(cli: Cli, command: &[OsString]) -> i32 {
    let (program, child_args) = match command.split_first() {
        Some(parts) => parts,
        None => {
            eprintln!(
                "run-hidden-rs: no program given. Usage: run-hidden-rs [OPTIONS] -- <program> [args...]"
            );
            return 2;
        }
    };

    let mut command = Command::new(program);
    command.args(child_args);

    // Wire up the child's stdio. A path redirects the stream to that file; its
    // absence means "forward to our own stream" (for stdout/stderr) or "null"
    // (for stdin). Files are opened up front so we fail before spawning.
    match &cli.stdin_path {
        Some(path) => match File::open(path) {
            Ok(file) => {
                command.stdin(file);
            }
            Err(err) => {
                eprintln!("run-hidden-rs: cannot open stdin file {}: {err}", path.display());
                return 1;
            }
        },
        None => {
            command.stdin(Stdio::null());
        }
    }

    let pump_stdout = match configure_output(&mut command, &cli.stdout_path, OutKind::Stdout) {
        Ok(pump) => pump,
        Err(code) => return code,
    };
    let pump_stderr = match configure_output(&mut command, &cli.stderr_path, OutKind::Stderr) {
        Ok(pump) => pump,
        Err(code) => return code,
    };

    // Hide the console window on Windows.
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let child = match SharedChild::spawn(&mut command) {
        Ok(child) => Arc::new(child),
        Err(err) => {
            eprintln!(
                "run-hidden-rs: failed to launch {}: {err}",
                program.to_string_lossy()
            );
            // 127 == "command not found"-ish, mirroring shells.
            return 127;
        }
    };

    // Make sure the child dies if we are terminated. `ctrlc` runs this handler
    // on its own dedicated thread, so calling `kill()` here is safe even though
    // the main thread is parked in `wait()`. With the `termination` feature this
    // also fires for SIGTERM/SIGHUP (Unix) and Ctrl-Break/console-close (Windows).
    {
        let child = Arc::clone(&child);
        if let Err(err) = ctrlc::set_handler(move || {
            let _ = child.kill();
        }) {
            eprintln!("run-hidden-rs: warning: could not install signal handler: {err}");
        }
    }

    // Pump any piped streams (i.e. the ones not redirected to a file) through
    // ours on background threads so they drain concurrently without deadlocking.
    let mut pumps = Vec::new();
    if pump_stdout && let Some(mut out) = child.take_stdout() {
        pumps.push(thread::spawn(move || {
            let _ = io::copy(&mut out, &mut io::stdout());
        }));
    }
    if pump_stderr && let Some(mut err) = child.take_stderr() {
        pumps.push(thread::spawn(move || {
            let _ = io::copy(&mut err, &mut io::stderr());
        }));
    }

    let status = match child.wait() {
        Ok(status) => status,
        Err(err) => {
            eprintln!("run-hidden-rs: failed to wait for child: {err}");
            return 1;
        }
    };

    // Let the pumps finish flushing whatever is left in the pipes.
    for pump in pumps {
        let _ = pump.join();
    }

    status_to_code(status)
}

enum OutKind {
    Stdout,
    Stderr,
}

/// Point the child's stdout/stderr at a file when a path is given, otherwise at
/// a pipe we will forward ourselves. Returns `Ok(true)` when the caller should
/// spawn a pump thread for this stream, `Ok(false)` when it goes straight to a
/// file, or `Err(exit_code)` if the file could not be created.
fn configure_output(
    command: &mut Command,
    path: &Option<PathBuf>,
    kind: OutKind,
) -> Result<bool, i32> {
    match path {
        Some(path) => match File::create(path) {
            Ok(file) => {
                match kind {
                    OutKind::Stdout => command.stdout(file),
                    OutKind::Stderr => command.stderr(file),
                };
                Ok(false)
            }
            Err(err) => {
                let which = match kind {
                    OutKind::Stdout => "stdout",
                    OutKind::Stderr => "stderr",
                };
                eprintln!(
                    "run-hidden-rs: cannot create {which} file {}: {err}",
                    path.display()
                );
                Err(1)
            }
        },
        None => {
            match kind {
                OutKind::Stdout => command.stdout(Stdio::piped()),
                OutKind::Stderr => command.stderr(Stdio::piped()),
            };
            Ok(true)
        }
    }
}

/// Reattach to the parent process's console, if it has one, so output we forward
/// to our own stdout/stderr is visible when launched from a terminal. Attaching
/// to an existing console never creates a window; if there is no parent console
/// the call simply fails and we stay window-less.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: AttachConsole has no preconditions; it returns 0 when there is no
    // parent console, which we intentionally ignore.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Turn an `ExitStatus` into a process exit code, mapping signal-kills to the
/// conventional `128 + signal` so the caller can tell what happened.
fn status_to_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}
