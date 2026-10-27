use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use linkrd::{
    clipboard::{ClipboardMonitor, SystemClipboard},
    install, logging, serial,
    transport::{ConnectionOptions, run_connection},
};
use serialport::{SerialPortInfo, SerialPortType};

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const DEFAULT_VID: u16 = 0x1D6B;
const DEFAULT_PID: u16 = 0x0104;

#[derive(Parser, Debug)]
#[command(
    version,
    args_conflicts_with_subcommands = true,
    about = "USB ACM agent for bidirectional text clipboard synchronization"
)]
struct Args {
    #[command(subcommand)]
    command: Option<ManagementCommand>,
    #[arg(help = "Run with saved installation arguments and retry desktop initialization")]
    #[arg(long, exclusive = true)]
    startup: bool,
    #[arg(help = "Log verbosity: off, error, warn, info, debug, trace")]
    #[arg(long, default_value = "info")]
    log_level: tracing::level_filters::LevelFilter,
    #[arg(help = "Log directory (default: local application data/linkrd/logs)")]
    #[arg(long, conflicts_with = "no_file_log")]
    log_dir: Option<std::path::PathBuf>,
    #[arg(help = "Only log to stderr; do not create log files")]
    #[arg(long)]
    no_file_log: bool,
    #[arg(help = "Serial device, e.g. /dev/ttyACM0, /dev/cu.usbmodem..., COM3")]
    #[arg(long, conflicts_with_all = ["vid", "pid", "serial_number"])]
    port: Option<String>,
    #[arg(help = "Override USB VID in hex (default 1D6B). Requires --pid")]
    #[arg(long, value_parser = parse_hex, requires = "pid")]
    vid: Option<u16>,
    #[arg(help = "Override USB PID in hex (default 0104). Requires --vid")]
    #[arg(long, value_parser = parse_hex, requires = "vid")]
    pid: Option<u16>,
    #[arg(help = "Optional USB serial number to distinguish devices with the same VID/PID")]
    #[arg(long)]
    serial_number: Option<String>,
    #[arg(help = "List detected serial ports and exit (no clipboard access required)")]
    #[arg(long)]
    list_ports: bool,
    #[arg(help = "Conventional ACM line coding; not the USB transfer speed limit")]
    #[arg(long, default_value_t = 115200, value_parser = clap::value_parser!(u32).range(1..))]
    baud: u32,
}

#[derive(Debug, Subcommand)]
enum ManagementCommand {
    #[command(about = "Start the installed executable in the background with saved arguments")]
    Start,
    #[command(about = "Request graceful shutdown and wait for the current user's instance to stop")]
    Stop {
        #[arg(help = "Maximum seconds to wait; does not forcibly terminate on timeout")]
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    #[command(
        about = "Print the current user's running linkrd PID (nonzero exit if not available)"
    )]
    Pid,
    #[command(about = "Show whether linkrd is running and its PID (not desktop/ACM health)")]
    Status,
    #[command(
        about = "Install this executable for the current user and start it in the background"
    )]
    Install {
        #[arg(help = "Program directory (default: ~/.local/bin or LOCALAPPDATA/Programs/linkrd)")]
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        #[arg(help = "Register login autostart after installation")]
        #[arg(long)]
        autostart: bool,
        #[arg(help = "Only install; do not start the background process now")]
        #[arg(long)]
        no_start: bool,
        #[arg(help = "Runtime options to save, after --. Omit to preserve existing settings")]
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Remove the installed executable and login entry; retain logs")]
    Uninstall,
    #[command(about = "Manage login autostart for the installed executable")]
    Autostart {
        #[command(subcommand)]
        action: AutostartCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AutostartCommand {
    #[command(about = "Register for the next login; optional runtime options follow --")]
    Enable {
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Remove the login entry without stopping a running process")]
    Disable,
    #[command(about = "Inspect registration (not process health)")]
    Status,
}

fn runtime_args(arguments: &[String]) -> Result<Args> {
    let args = Args::try_parse_from(
        std::iter::once("linkrd").chain(arguments.iter().map(String::as_str)),
    )?;
    anyhow::ensure!(
        !args.startup && args.command.is_none() && !args.list_ports,
        "startup arguments must contain only daemon options, not commands, --startup or --list-ports"
    );
    Ok(args)
}

fn saved_args(arguments: &[String]) -> Result<Option<Vec<String>>> {
    if arguments.is_empty() {
        return Ok(None);
    }
    let args = runtime_args(arguments)?;
    let mut result = vec![
        "--log-level".into(),
        args.log_level.to_string().to_lowercase(),
        "--baud".into(),
        args.baud.to_string(),
    ];
    if let Some(path) = args.log_dir {
        let absolute = std::path::absolute(path)?;
        result.extend([
            "--log-dir".into(),
            absolute
                .to_str()
                .context("startup paths must be valid Unicode")?
                .into(),
        ]);
    }
    if args.no_file_log {
        result.push("--no-file-log".into());
    }
    if let Some(port) = args.port {
        #[cfg(unix)]
        let port = std::path::absolute(port)?
            .to_str()
            .context("serial path must be valid Unicode")?
            .to_owned();
        result.extend(["--port".into(), port]);
    }
    for (flag, value) in [("--vid", args.vid), ("--pid", args.pid)] {
        if let Some(value) = value {
            result.extend([flag.into(), format!("{value:04x}")]);
        }
    }
    if let Some(serial) = args.serial_number {
        result.extend(["--serial-number".into(), serial]);
    }
    Ok(Some(result))
}

fn manage(command: &ManagementCommand) -> Result<()> {
    let directory = install::config_dir()?;
    let _management = install::lock(&directory, "management.lock")?;
    match command {
        ManagementCommand::Start => match install::runtime_status(&directory)? {
            install::RuntimeStatus::Running(pid) => println!("already running: pid={pid}"),
            install::RuntimeStatus::Stopped => {
                let pid = install::start(&directory)?;
                println!(
                    "background process launched: pid={pid}; uses saved arguments; check logs for desktop/ACM readiness"
                );
            }
            install::RuntimeStatus::PidUnavailable => anyhow::bail!(
                "runtime lock is held but PID unavailable; check the existing process before starting"
            ),
        },
        ManagementCommand::Stop { timeout } => {
            match install::stop(&directory, Duration::from_secs(*timeout))? {
                Some(pid) => println!("stopped: pid={pid}"),
                None => println!("already stopped"),
            }
        }
        ManagementCommand::Pid => match install::runtime_status(&directory)? {
            install::RuntimeStatus::Running(pid) => println!("{pid}"),
            install::RuntimeStatus::Stopped => anyhow::bail!("linkrd is not running"),
            install::RuntimeStatus::PidUnavailable => anyhow::bail!(
                "runtime lock is held but PID is unavailable; restart linkrd with the updated binary"
            ),
        },
        ManagementCommand::Status => match install::runtime_status(&directory)? {
            install::RuntimeStatus::Running(pid) => println!("running: pid={pid}"),
            install::RuntimeStatus::Stopped => println!("stopped"),
            install::RuntimeStatus::PidUnavailable => {
                println!("runtime lock is held; PID unavailable (restart with the updated binary)")
            }
        },
        ManagementCommand::Install {
            dir,
            autostart,
            no_start,
            args,
        } => {
            let args = saved_args(args)?;
            let runtime = install::lock(&directory, "runtime.lock")?;
            let installed = install::install(&directory, dir.as_deref(), args)?;
            println!("installed: {}", installed.executable.display());
            install::print_path_instructions(&installed)?;
            if *autostart {
                install::enable_autostart(&directory, None)?;
                println!("login autostart registered; takes effect on next login");
            }
            drop(runtime);
            if !no_start {
                let pid = install::start_background(&installed.executable)?;
                println!(
                    "background process launched: pid={pid}; uses saved arguments; check logs for desktop/ACM readiness"
                );
            } else {
                println!("Process not started (--no-start).");
            }
        }
        ManagementCommand::Uninstall => {
            let _runtime = install::lock(&directory, "runtime.lock")?;
            install::uninstall(&directory)?;
        }
        ManagementCommand::Autostart { action } => match action {
            AutostartCommand::Enable { args } => {
                install::enable_autostart(&directory, saved_args(args)?)?;
                println!("login autostart registered; takes effect on next login");
            }
            AutostartCommand::Disable => {
                install::disable_autostart(&directory)?;
                println!("login autostart removed; existing processes are not stopped");
            }
            AutostartCommand::Status => install::autostart_status(&directory)?,
        },
    }
    Ok(())
}

fn parse_hex(value: &str) -> Result<u16, String> {
    u16::from_str_radix(value.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .map_err(|_| "expected a 16-bit hexadecimal USB ID".into())
}

fn main() -> std::process::ExitCode {
    let mut args = Args::parse();
    if let Some(command) = &args.command {
        return match manage(command) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("linkrd: {error:#}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if args.startup {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Console::FreeConsole();
        }
        match install::startup_args().and_then(|saved| runtime_args(&saved)) {
            Ok(mut saved) => {
                saved.startup = true;
                args = saved;
            }
            Err(error) => {
                report_startup_error(&error.context("cannot load startup configuration"));
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let directory = args
        .log_dir
        .clone()
        .unwrap_or_else(logging::default_log_dir);
    let _logging = match logging::init(
        args.log_level,
        (!args.no_file_log).then_some(directory.as_path()),
    ) {
        Ok(guard) => guard,
        Err(error) => {
            if args.startup {
                report_startup_error(&error.context("cannot initialize configured logging"));
            } else {
                eprintln!("linkrd: {error:#}");
            }
            return std::process::ExitCode::FAILURE;
        }
    };
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        "linkrd starting"
    );
    match run(&args) {
        Ok(()) => {
            tracing::info!("linkrd stopped");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(error = %format_args!("{error:#}"), "linkrd stopped with an error");
            std::process::ExitCode::FAILURE
        }
    }
}

fn report_startup_error(error: &anyhow::Error) {
    match logging::init(
        tracing::level_filters::LevelFilter::INFO,
        Some(&logging::default_log_dir()),
    ) {
        Ok(_guard) => {
            tracing::error!(error = %format_args!("{error:#}"), "linkrd login startup failed")
        }
        Err(log_error) => {
            eprintln!("linkrd: {error:#}; cannot write startup diagnostics: {log_error:#}")
        }
    }
}

fn run(args: &Args) -> Result<()> {
    if args.list_ports {
        for port in serialport::available_ports().context("cannot enumerate serial ports")? {
            println!("{} {:?}", port.port_name, port.port_type);
        }
        return Ok(());
    }
    let instance = install::acquire_runtime(&install::config_dir()?)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let _control = instance.monitor_shutdown(Arc::clone(&shutdown))?;
    if !(cfg!(windows) && args.startup) {
        let signal_shutdown = Arc::clone(&shutdown);
        ctrlc::set_handler(move || signal_shutdown.store(true, Ordering::Release))
            .context("cannot register Ctrl-C handler")?;
    }
    let dirty = Arc::new(AtomicBool::new(true));
    let Some((mut clipboard, watcher)) = initialize_desktop(args.startup, &shutdown, || {
        let clipboard = SystemClipboard::new()?;
        let watcher = ClipboardMonitor::start(Arc::clone(&dirty))?;
        Ok((clipboard, watcher))
    })?
    else {
        return Ok(());
    };
    tracing::info!("clipboard backend and watcher initialized");
    let mut reconnect_delay = Duration::from_millis(500);
    let mut watcher_reported = false;
    while !shutdown.load(Ordering::Acquire) {
        if watcher.is_finished() && !watcher_reported {
            tracing::warn!("clipboard watcher stopped; continuing with periodic refresh");
            watcher_reported = true;
        }
        let started = Instant::now();
        let result = (|| -> Result<()> {
            let name = resolve_port(args)?;
            tracing::debug!(port = %name, baud = args.baud, "opening ACM port");
            let mut writer = serial::open(&name, args.baud)?;
            writer
                .write_data_terminal_ready(true)
                .context("cannot assert ACM DTR")?;
            writer
                .clear(serialport::ClearBuffer::All)
                .context("cannot clear stale ACM data")?;
            let mut reader = writer.try_clone().context("cannot clone ACM port")?;
            tracing::info!(port = %name, "ACM connected; waiting for Hello");
            let result = run_connection(
                &mut reader,
                &mut writer,
                &mut clipboard,
                &dirty,
                &shutdown,
                ConnectionOptions::default(),
            );
            serial::close(reader, writer);
            result
        })();
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if started.elapsed() >= Duration::from_secs(10) {
            reconnect_delay = Duration::from_millis(500);
        }
        if let Err(error) = result {
            tracing::warn!(error = %format_args!("{error:#}"), retry_ms = reconnect_delay.as_millis() as u64, "ACM unavailable; reconnecting");
        }
        let retry_at = Instant::now() + reconnect_delay;
        while Instant::now() < retry_at && !shutdown.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(50));
        }
        reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(5));
    }
    tracing::info!("shutdown requested; stopping clipboard watcher");
    drop(watcher);
    Ok(())
}

fn initialize_desktop<T>(
    retry: bool,
    shutdown: &AtomicBool,
    mut initialize: impl FnMut() -> Result<T>,
) -> Result<Option<T>> {
    let mut delay = Duration::from_secs(1);
    while !shutdown.load(Ordering::Acquire) {
        match initialize() {
            Ok(desktop) => return Ok(Some(desktop)),
            Err(error) if retry => {
                tracing::warn!(error = %format_args!("{error:#}"), retry_ms = delay.as_millis() as u64,
                    "desktop initialization failed; waiting for user session");
                let deadline = Instant::now() + delay;
                while Instant::now() < deadline && !shutdown.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(50));
                }
                delay = (delay * 2).min(Duration::from_secs(30));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn resolve_port(args: &Args) -> Result<String> {
    if let Some(port) = &args.port {
        return Ok(port.clone());
    }
    select_usb_port(args, serialport::available_ports()?)
}

fn select_usb_port(args: &Args, ports: Vec<SerialPortInfo>) -> Result<String> {
    let candidates: Vec<SerialPortInfo> = ports
        .into_iter()
        .filter(|port| {
            if let SerialPortType::UsbPort(usb) = &port.port_type {
                usb.vid == args.vid.unwrap_or(DEFAULT_VID)
                    && usb.pid == args.pid.unwrap_or(DEFAULT_PID)
                    && args
                        .serial_number
                        .as_ref()
                        .is_none_or(|serial| usb.serial_number.as_ref() == Some(serial))
            } else {
                false
            }
        })
        .collect();
    if candidates.len() > 1 {
        tracing::debug!(
            matches = candidates.len(),
            "multiple USB ports matched; selecting first"
        );
    }
    candidates
        .first()
        .map(|port| port.port_name.clone())
        .ok_or_else(|| anyhow::anyhow!("no matching USB serial port found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::UsbPortInfo;

    #[test]
    fn management_cli_rejects_mixed_modes_and_recursive_startup() {
        assert!(Args::try_parse_from(["linkrd", "install", "--autostart"]).is_ok());
        assert!(Args::try_parse_from(["linkrd", "--startup"]).is_ok());
        assert!(Args::try_parse_from(["linkrd", "--port", "COM3", "install"]).is_err());
        assert!(Args::try_parse_from(["linkrd", "--startup", "--no-file-log"]).is_err());
        for invalid in ["install", "uninstall", "--startup", "--list-ports"] {
            assert!(saved_args(&[invalid.into()]).is_err());
        }
        let saved = saved_args(&[
            "--log-dir".into(),
            "my logs".into(),
            "--log-level".into(),
            "debug".into(),
        ])
        .unwrap()
        .unwrap();
        let parsed = runtime_args(&saved).unwrap();
        assert!(parsed.log_dir.unwrap().is_absolute());
        assert!(Args::try_parse_from(["linkrd", "--receive-dir", "unused"]).is_err());
        assert_eq!(parsed.log_level, tracing::level_filters::LevelFilter::DEBUG);
    }

    #[test]
    fn startup_retries_desktop_failure_and_cancellation_is_prompt() {
        let shutdown = AtomicBool::new(false);
        let mut attempts = 0;
        let result = initialize_desktop(true, &shutdown, || {
            attempts += 1;
            if attempts == 1 {
                anyhow::bail!("desktop not ready");
            }
            Ok(42)
        })
        .unwrap();
        assert_eq!(result, Some(42));
        assert_eq!(attempts, 2);
        let before = Instant::now();
        let result: Option<()> = initialize_desktop(true, &shutdown, || {
            shutdown.store(true, Ordering::Release);
            anyhow::bail!("cancel during initialization")
        })
        .unwrap();
        assert_eq!(result, None);
        assert!(before.elapsed() < Duration::from_millis(500));
        shutdown.store(false, Ordering::Release);
        assert!(initialize_desktop::<()>(false, &shutdown, || anyhow::bail!("fatal")).is_err());
    }

    fn usb(name: &str, vid: u16, pid: u16, serial: &str) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.into(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid,
                serial_number: Some(serial.into()),
                manufacturer: None,
                product: None,
            }),
        }
    }

    #[test]
    fn default_ids_select_only_the_matching_usb_device() {
        let args = Args::try_parse_from(["linkrd"]).unwrap();
        let ports = vec![
            usb("wrong", 0x1234, 0x0104, "a"),
            usb("COM3", 0x1D6B, 0x0104, "b"),
        ];
        assert_eq!(select_usb_port(&args, ports).unwrap(), "COM3");
        assert!(select_usb_port(&args, vec![usb("wrong", 0x1D6B, 0x0105, "a")]).is_err());
    }

    #[test]
    fn multiple_devices_choose_first_and_serial_can_override() {
        let ports = vec![
            usb("COM3", 0x1D6B, 0x0104, "a"),
            usb("COM4", 0x1D6B, 0x0104, "b"),
        ];
        assert_eq!(
            select_usb_port(&Args::try_parse_from(["linkrd"]).unwrap(), ports.clone()).unwrap(),
            "COM3"
        );
        let args = Args::try_parse_from(["linkrd", "--serial-number", "b"]).unwrap();
        assert_eq!(select_usb_port(&args, ports).unwrap(), "COM4");
    }

    #[test]
    fn explicit_selection_and_invalid_cli_options() {
        let args = Args::try_parse_from(["linkrd", "--port", "/dev/ttyACM0"]).unwrap();
        assert_eq!(resolve_port(&args).unwrap(), "/dev/ttyACM0");
        let args = Args::try_parse_from(["linkrd", "--vid", "0x1234", "--pid", "ABCD"]).unwrap();
        assert_eq!(
            select_usb_port(&args, vec![usb("other", 0x1234, 0xABCD, "a")]).unwrap(),
            "other"
        );
        assert!(Args::try_parse_from(["linkrd", "--vid", "1d6b"]).is_err());
        assert!(Args::try_parse_from(["linkrd", "--baud", "0"]).is_err());
        assert!(
            Args::try_parse_from(["linkrd", "--port", "COM3", "--serial-number", "a"]).is_err()
        );
    }
}
