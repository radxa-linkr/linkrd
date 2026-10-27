mod path;
mod startup;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;

#[derive(Debug, Serialize, Deserialize)]
pub struct Installation {
    format_version: u32,
    pub executable: PathBuf,
    sha256: String,
    pub startup_args: Vec<String>,
    #[serde(default)]
    command_path: path::Configuration,
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("cannot locate user configuration directory")?;
    ensure!(
        dir.is_absolute(),
        "user configuration directory must be absolute"
    );
    Ok(dir.join("linkrd"))
}

pub fn default_install_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let dir = dirs::data_local_dir()
        .context("cannot locate local application directory")?
        .join("Programs/linkrd");
    #[cfg(not(windows))]
    let dir = dirs::home_dir()
        .context("cannot locate home directory")?
        .join(".local/bin");
    ensure!(dir.is_absolute(), "user program directory must be absolute");
    Ok(dir)
}

pub fn lock(directory: &Path, name: &str) -> Result<File> {
    fs::create_dir_all(directory).context("cannot create linkrd configuration directory")?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(name))?;
    file.try_lock().with_context(|| {
        if name == "runtime.lock" {
            "linkrd is already running (or an installation is in progress); stop it before retrying"
        } else {
            "another linkrd management command is in progress"
        }
    })?;
    Ok(file)
}

fn state_lock(directory: &Path) -> Result<File> {
    fs::create_dir_all(directory)?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("runtime-state.lock"))?;
    file.lock().context("cannot lock runtime state")?;
    Ok(file)
}

pub struct RuntimeGuard {
    lock: File,
    directory: PathBuf,
    control: RuntimeControl,
}

#[derive(Serialize, Deserialize)]
struct RuntimeControl {
    pid: u32,
    instance_id: String,
}

pub struct ShutdownMonitor {
    finished: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for ShutdownMonitor {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl RuntimeGuard {
    pub fn monitor_shutdown(&self, shutdown: Arc<AtomicBool>) -> Result<ShutdownMonitor> {
        let finished = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&finished);
        let path = self.directory.join("runtime.stop");
        let instance_id = self.control.instance_id.clone();
        let thread = thread::Builder::new()
            .name("linkrd-control".into())
            .spawn(move || {
                let mut reported_error = false;
                while !cancelled.load(Ordering::Acquire) && !shutdown.load(Ordering::Acquire) {
                    match fs::read_to_string(&path) {
                        Ok(request) if request == instance_id => {
                            tracing::info!("stop requested by local command");
                            shutdown.store(true, Ordering::Release);
                            break;
                        }
                        Err(error)
                            if error.kind() != io::ErrorKind::NotFound && !reported_error =>
                        {
                            tracing::warn!(%error, "cannot read local stop request");
                            reported_error = true;
                        }
                        _ => (),
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            })
            .context("cannot start local command monitor")?;
        Ok(ShutdownMonitor {
            finished,
            thread: Some(thread),
        })
    }
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        if let Ok(_state) = state_lock(&self.directory) {
            let _ = fs::remove_file(self.directory.join("runtime.pid"));
            let _ = fs::remove_file(self.directory.join("runtime.control.json"));
            let _ = fs::remove_file(self.directory.join("runtime.stop"));
            let _ = self.lock.unlock();
        }
    }
}

pub fn acquire_runtime(directory: &Path) -> Result<RuntimeGuard> {
    let _state = state_lock(directory)?;
    let lock = lock(directory, "runtime.lock")?;
    let control = RuntimeControl {
        pid: std::process::id(),
        instance_id: uuid::Uuid::new_v4().to_string(),
    };
    match fs::remove_file(directory.join("runtime.stop")) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error).context("cannot clear obsolete stop request"),
    }
    atomic_write(
        &directory.join("runtime.control.json"),
        &serde_json::to_vec(&control)?,
    )?;
    atomic_write(
        &directory.join("runtime.pid"),
        format!("{}\n", std::process::id()).as_bytes(),
    )
    .context("cannot publish linkrd PID")?;
    Ok(RuntimeGuard {
        lock,
        directory: directory.to_path_buf(),
        control,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeStatus {
    Stopped,
    Running(u32),
    PidUnavailable,
}

pub fn runtime_status(directory: &Path) -> Result<RuntimeStatus> {
    let _state = state_lock(directory)?;
    runtime_status_locked(directory)
}

fn runtime_status_locked(directory: &Path) -> Result<RuntimeStatus> {
    let file = match File::options()
        .read(true)
        .write(true)
        .open(directory.join("runtime.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(RuntimeStatus::Stopped),
        Err(error) => return Err(error).context("cannot inspect runtime lock"),
    };
    match file.try_lock() {
        Ok(()) => return Ok(RuntimeStatus::Stopped),
        Err(fs::TryLockError::WouldBlock) => (),
        Err(fs::TryLockError::Error(error)) => {
            return Err(error).context("cannot inspect runtime lock");
        }
    }
    let pid = match fs::read_to_string(directory.join("runtime.pid")) {
        Ok(pid) => pid.trim().parse::<u32>().ok().filter(|pid| *pid > 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("cannot read runtime PID"),
    };
    Ok(pid.map_or(RuntimeStatus::PidUnavailable, RuntimeStatus::Running))
}

fn read_control(directory: &Path, pid: u32) -> Result<RuntimeControl> {
    let control: RuntimeControl = serde_json::from_slice(&fs::read(directory.join("runtime.control.json"))
        .context("running instance does not support local stop or its control record is missing; stop it manually once and run the updated binary")?)?;
    ensure!(
        control.pid == pid && uuid::Uuid::parse_str(&control.instance_id).is_ok(),
        "runtime control record does not match the running instance; refusing to send stop request"
    );
    Ok(control)
}

pub fn stop(directory: &Path, timeout: Duration) -> Result<Option<u32>> {
    let control = {
        let _state = state_lock(directory)?;
        let pid = match runtime_status_locked(directory)? {
            RuntimeStatus::Stopped => return Ok(None),
            RuntimeStatus::Running(pid) => pid,
            RuntimeStatus::PidUnavailable => {
                bail!("PID unavailable; stop the old instance manually and run the updated binary")
            }
        };
        let control = read_control(directory, pid)?;
        atomic_write(
            &directory.join("runtime.stop"),
            control.instance_id.as_bytes(),
        )
        .context("cannot request linkrd shutdown")?;
        control
    };
    let deadline = Instant::now() + timeout;
    loop {
        {
            let _state = state_lock(directory)?;
            match runtime_status_locked(directory)? {
                RuntimeStatus::Stopped => return Ok(Some(control.pid)),
                RuntimeStatus::Running(pid) => {
                    if read_control(directory, pid)?.instance_id != control.instance_id {
                        return Ok(Some(control.pid));
                    }
                }
                RuntimeStatus::PidUnavailable => {
                    bail!("runtime changed while waiting for shutdown; check `linkrd status`")
                }
            }
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for linkrd (pid={}) to stop; request remains pending; check logs and retry `linkrd stop`",
            control.pid
        );
        thread::sleep(Duration::from_millis(50));
    }
}

pub fn start(directory: &Path) -> Result<u32> {
    let installed = require_installation(directory)?;
    ensure!(
        verify_owned(&installed)?,
        "installed executable is missing; run `linkrd install` again"
    );
    start_background(&installed.executable)
}

fn manifest_path(directory: &Path) -> PathBuf {
    directory.join("installation.json")
}

pub fn load(directory: &Path) -> Result<Option<Installation>> {
    let path = manifest_path(directory);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read installation record"),
    };
    let installation: Installation =
        serde_json::from_slice(&bytes).context("invalid installation record")?;
    ensure!(
        installation.format_version == 1,
        "unsupported installation record version"
    );
    ensure!(
        installation.executable.is_absolute()
            && installation.executable.file_name() == Some(executable_name().as_ref()),
        "invalid executable path in installation record"
    );
    Ok(Some(installation))
}

pub fn require_installation(directory: &Path) -> Result<Installation> {
    load(directory)?.context("linkrd is not installed for this user; run `linkrd install` first")
}

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "linkrd.exe"
    } else {
        "linkrd"
    }
}

pub(super) fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().context("file has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut staged = NamedTempFile::new_in(parent)?;
    staged.write_all(data)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn save(directory: &Path, installation: &Installation) -> Result<()> {
    atomic_write(
        &manifest_path(directory),
        &serde_json::to_vec_pretty(installation)?,
    )
    .context("cannot save installation record")
}

fn fingerprint(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    io::copy(&mut File::open(path)?, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

fn verify_owned(installation: &Installation) -> Result<bool> {
    let metadata = match fs::symlink_metadata(&installation.executable) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file(),
        "installed executable was replaced with a non-regular file; refusing to modify it"
    );
    ensure!(
        fingerprint(&installation.executable)? == installation.sha256,
        "installed executable was modified outside `linkrd install`; refusing to overwrite or delete it"
    );
    Ok(true)
}

fn stage_binary(source: &Path, directory: &Path) -> Result<NamedTempFile> {
    let mut staged = NamedTempFile::new_in(directory)?;
    io::copy(&mut File::open(source)?, &mut staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    staged.as_file().sync_all()?;
    Ok(staged)
}

pub fn install(
    directory: &Path,
    destination: Option<&Path>,
    args: Option<Vec<String>>,
) -> Result<Installation> {
    let previous = load(directory)?;
    let destination = match destination {
        Some(path) => path.to_path_buf(),
        None => match &previous {
            Some(installed) => installed.executable.parent().unwrap().to_path_buf(),
            None => default_install_dir()?,
        },
    };
    fs::create_dir_all(&destination).context("cannot create program directory")?;
    let destination = fs::canonicalize(destination)?;
    let executable = destination.join(executable_name());
    let owned = if let Some(previous) = &previous {
        ensure!(
            previous.executable == executable,
            "linkrd is already installed at {}; uninstall before changing the installation directory",
            previous.executable.display()
        );
        verify_owned(previous)?
    } else {
        ensure!(
            fs::symlink_metadata(&executable).is_err_and(|e| e.kind() == io::ErrorKind::NotFound),
            "destination {} already exists or cannot be inspected; refusing to overwrite an unmanaged file",
            executable.display()
        );
        false
    };
    let source = std::env::current_exe().context("cannot locate running executable")?;
    let staged = stage_binary(&source, &destination)?;
    let command_path = previous
        .as_ref()
        .map(|p| p.command_path.clone())
        .unwrap_or_default();
    let mut installation = Installation {
        format_version: 1,
        executable,
        sha256: fingerprint(staged.path())?,
        startup_args: args.unwrap_or_else(|| previous.map(|p| p.startup_args).unwrap_or_default()),
        command_path,
    };
    let backup = if owned {
        Some(stage_binary(&installation.executable, &destination)?)
    } else {
        None
    };
    let unchanged = owned && fingerprint(&installation.executable)? == installation.sha256;
    if !unchanged {
        staged
            .persist(&installation.executable)
            .map_err(|error| error.error)
            .context("cannot install executable; stop other linkrd processes and retry")?;
    }
    if let Err(error) = save(directory, &installation) {
        if !unchanged {
            let rollback = match backup {
                Some(backup) => backup
                    .persist(&installation.executable)
                    .map(|_| ())
                    .map_err(|e| e.error),
                None => fs::remove_file(&installation.executable),
            };
            rollback.context(format!(
                "{error:#}; additionally could not roll back executable"
            ))?;
        }
        return Err(error);
    }
    let configure_path = (|| -> Result<()> {
        installation.command_path = path::plan(&destination, &installation.command_path)?;
        save(directory, &installation)?;
        path::enable(&destination, &installation.command_path)
    })();
    configure_path.context(
        "cannot configure command PATH; installation is retained; fix the error and retry `linkrd install`",
    )?;
    Ok(installation)
}

pub fn print_path_instructions(installation: &Installation) -> Result<()> {
    path::print_instructions(installation.executable.parent().unwrap())
}

pub fn enable_autostart(directory: &Path, args: Option<Vec<String>>) -> Result<()> {
    let mut installed = require_installation(directory)?;
    ensure!(
        verify_owned(&installed)?,
        "installed executable is missing; run `linkrd install` again"
    );
    if let Some(args) = args {
        installed.startup_args = args;
        save(directory, &installed)?;
    }
    startup::enable(&installed.executable)
        .context("cannot register login autostart; installation is retained, fix the error and retry `linkrd autostart enable`")
}

pub fn disable_autostart(directory: &Path) -> Result<()> {
    if let Some(installed) = load(directory)? {
        startup::disable(&installed.executable)?;
    }
    Ok(())
}

pub fn autostart_status(directory: &Path) -> Result<()> {
    let Some(installed) = load(directory)? else {
        println!("not installed for this user");
        return Ok(());
    };
    println!("executable: {}", installed.executable.display());
    println!("executable present: {}", installed.executable.is_file());
    println!("autostart: {}", startup::status(&installed.executable)?);
    println!(
        "startup arguments: {}",
        serde_json::to_string(&installed.startup_args)?
    );
    println!("Registration status only; desktop/OS overrides and process health are not checked.");
    Ok(())
}

pub fn uninstall(directory: &Path) -> Result<()> {
    let Some(installed) = load(directory)? else {
        println!("linkrd is not installed for this user");
        return Ok(());
    };
    let present = verify_owned(&installed)?;
    startup::disable(&installed.executable)
        .context("cannot remove autostart; installation retained")?;
    path::disable(
        installed.executable.parent().unwrap(),
        &installed.command_path,
    )
    .context("cannot remove command PATH configuration; installation retained")?;
    if present {
        let current = fs::canonicalize(std::env::current_exe()?)?;
        if current == fs::canonicalize(&installed.executable)? {
            self_replace::self_delete().context("cannot schedule executable removal")?;
        } else {
            fs::remove_file(&installed.executable).context(
                "cannot remove executable; stop other linkrd processes and retry uninstall",
            )?;
        }
    }
    fs::remove_file(manifest_path(directory)).context(
        "executable removed but installation record could not be removed; retry uninstall",
    )?;
    println!(
        "uninstalled {}; logs retained",
        installed.executable.display()
    );
    Ok(())
}

pub fn startup_args() -> Result<Vec<String>> {
    let installed = require_installation(&config_dir()?)?;
    if fs::canonicalize(std::env::current_exe()?)? != fs::canonicalize(installed.executable)? {
        bail!("--startup must be run from the installed executable");
    }
    Ok(installed.startup_args)
}

pub fn start_background(executable: &Path) -> Result<u32> {
    let mut command = Command::new(executable);
    command
        .arg("--startup")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| rustix::process::setsid().map(|_| ()).map_err(Into::into));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = command.spawn().context(
        "installed but could not start background process; installation retained; run the installed executable with --startup to diagnose",
    )?;
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        if let Some(status) = child
            .try_wait()
            .context("cannot inspect background process")?
        {
            bail!(
                "installed but background process exited ({status}); installation retained; check logs or run the installed executable with --startup"
            );
        }
        if Instant::now() >= deadline {
            return Ok(child.id());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn local_stop_ignores_other_instances_and_waits_for_runtime_release() {
        let root = tempfile::tempdir().unwrap();
        let runtime = acquire_runtime(root.path()).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let monitor = runtime.monitor_shutdown(Arc::clone(&shutdown)).unwrap();
        atomic_write(
            &root.path().join("runtime.stop"),
            uuid::Uuid::new_v4().to_string().as_bytes(),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(120));
        assert!(!shutdown.load(Ordering::Acquire));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !worker_shutdown.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            drop(monitor);
            drop(runtime);
        });
        let result = stop(root.path(), Duration::from_secs(2));
        worker.join().unwrap();
        assert_eq!(result.unwrap(), Some(std::process::id()));
        assert!(shutdown.load(Ordering::Acquire));
        assert_eq!(runtime_status(root.path()).unwrap(), RuntimeStatus::Stopped);
        assert_eq!(stop(root.path(), Duration::from_secs(1)).unwrap(), None);
    }

    #[test]
    fn stop_timeout_retains_request_but_restart_discards_it() {
        let root = tempfile::tempdir().unwrap();
        let runtime = acquire_runtime(root.path()).unwrap();
        let error = stop(root.path(), Duration::from_millis(20)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let request = fs::read_to_string(root.path().join("runtime.stop")).unwrap();
        assert_eq!(request, runtime.control.instance_id);
        assert_eq!(
            runtime_status(root.path()).unwrap(),
            RuntimeStatus::Running(std::process::id())
        );
        drop(runtime);
        atomic_write(&root.path().join("runtime.stop"), request.as_bytes()).unwrap();
        let next = acquire_runtime(root.path()).unwrap();
        assert_ne!(next.control.instance_id, request);
        assert!(!root.path().join("runtime.stop").exists());
    }
}
