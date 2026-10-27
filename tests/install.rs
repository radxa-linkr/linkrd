#![cfg(target_os = "linux")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Account(TempDir);

impl Account {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn path(&self, relative: &str) -> PathBuf {
        self.0.path().join(relative)
    }
    fn command(&self, executable: &Path) -> Command {
        let mut command = Command::new(executable);
        command
            .env("HOME", self.0.path())
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env_remove("ZDOTDIR")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .current_dir(self.0.path());
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(Path::new(env!("CARGO_BIN_EXE_linkrd")))
            .args(args)
            .output()
            .unwrap()
    }
    fn success(&self, args: &[&str]) -> Output {
        let result = self.run(args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result
    }
    fn manifest(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.path("config/linkrd/installation.json")).unwrap())
            .unwrap()
    }
}

#[test]
fn install_reinstall_autostart_and_self_uninstall_preserve_user_files() {
    let account = Account::new();
    let bin = account.path("my 程序 $`% bin");
    account.success(&[
        "install",
        "--no-start",
        "--dir",
        bin.to_str().unwrap(),
        "--autostart",
        "--",
        "--log-dir",
        "custom logs",
        "--log-level",
        "debug",
    ]);
    let exe = bin.join("linkrd");
    assert!(
        account
            .command(&exe)
            .arg("--version")
            .status()
            .unwrap()
            .success()
    );
    let manifest = account.manifest();
    assert!(
        manifest["startup_args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == account.path("custom logs").to_str())
    );
    let desktop = account.path("config/autostart/linkrd.desktop");
    let content = fs::read_to_string(&desktop).unwrap();
    assert!(content.contains(" --startup\n"));
    assert!(content.contains(r"\\$\\`%%"));
    let status = account.success(&["autostart", "status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("autostart: registered"));
    account.success(&["install", "--no-start"]);
    assert_eq!(account.manifest(), manifest);
    account.success(&["autostart", "disable"]);
    assert!(!desktop.exists());
    account.success(&["autostart", "disable"]);
    account.success(&["autostart", "enable"]);
    account.success(&["autostart", "enable"]);
    fs::write(bin.join("unrelated"), "keep").unwrap();
    fs::create_dir_all(account.path("unrelated files")).unwrap();
    fs::write(account.path("unrelated files/notes.txt"), "keep").unwrap();
    fs::create_dir_all(account.path("data/linkrd/logs")).unwrap();
    fs::write(account.path("data/linkrd/logs/linkrd.log"), "keep").unwrap();
    let result = account.command(&exe).arg("uninstall").output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!exe.exists());
    assert!(!desktop.exists());
    assert!(!account.path("config/linkrd/installation.json").exists());
    assert_eq!(fs::read_to_string(bin.join("unrelated")).unwrap(), "keep");
    assert_eq!(
        fs::read_to_string(account.path("unrelated files/notes.txt")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::read_to_string(account.path("data/linkrd/logs/linkrd.log")).unwrap(),
        "keep"
    );
    account.success(&["uninstall"]);
}

#[test]
fn installed_command_is_found_in_new_shells_and_path_setup_is_idempotent() {
    let account = Account::new();
    let bin = account.path("my 程序 ' $`*?\\ bin");
    let original = "# user configuration without final newline";
    fs::write(account.path(".profile"), original).unwrap();
    fs::write(account.path(".bash_profile"), original).unwrap();
    account.success(&["install", "--no-start", "--dir", bin.to_str().unwrap()]);
    let files = [
        ".profile",
        ".bashrc",
        ".bash_profile",
        ".zprofile",
        ".zshrc",
        "config/fish/conf.d/linkrd.fish",
    ];
    let profiles: Vec<_> = files
        .iter()
        .map(|file| fs::read_to_string(account.path(file)).unwrap())
        .collect();
    account.success(&["install", "--no-start"]);
    for (file, expected) in files.iter().zip(&profiles) {
        assert_eq!(fs::read_to_string(account.path(file)).unwrap(), *expected);
    }
    for script in [
        ". \"$HOME/.bashrc\"; . \"$HOME/.bashrc\"; command -v linkrd; linkrd --version; printf '%s' \"$PATH\"",
        ". \"$HOME/.profile\"; command -v linkrd; linkrd --version; printf '%s' \"$PATH\"",
        ". \"$HOME/.bash_profile\"; command -v linkrd; linkrd --version; printf '%s' \"$PATH\"",
        ". \"$HOME/.zshrc\"; . \"$HOME/.zprofile\"; command -v linkrd; linkrd --version; printf '%s' \"$PATH\"",
    ] {
        let result = account
            .command(Path::new("/bin/sh"))
            .env("PATH", "/usr/bin:/bin")
            .args(["-c", script])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stdout = String::from_utf8(result.stdout).unwrap();
        assert!(
            stdout.starts_with(bin.join("linkrd").to_str().unwrap()),
            "{stdout}"
        );
        assert!(stdout.contains(&format!("linkrd {}", env!("CARGO_PKG_VERSION"))));
        let path = stdout.lines().last().unwrap();
        assert_eq!(
            path.split(':')
                .filter(|entry| *entry == bin.to_str().unwrap())
                .count(),
            1
        );
    }
    let result = account
        .command(Path::new("/bin/bash"))
        .env("PATH", "/usr/bin:/bin")
        .args(["-ic", "command -v linkrd && linkrd --version"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).starts_with(bin.join("linkrd").to_str().unwrap())
    );
    account.success(&["uninstall"]);
    assert_eq!(
        fs::read_to_string(account.path(".profile")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(account.path(".bash_profile")).unwrap(),
        original
    );
    for file in files
        .iter()
        .filter(|file| **file != ".profile" && **file != ".bash_profile")
    {
        assert_eq!(fs::read_to_string(account.path(file)).unwrap(), "");
    }
}

#[test]
fn path_setup_preserves_profile_symlinks_permissions_and_user_edits() {
    use std::os::unix::{fs::PermissionsExt, fs::symlink};
    let account = Account::new();
    let dotfile = account.path("dotfile");
    fs::write(&dotfile, "# dotfile").unwrap();
    fs::set_permissions(&dotfile, fs::Permissions::from_mode(0o640)).unwrap();
    symlink(&dotfile, account.path(".bashrc")).unwrap();
    account.success(&["install", "--no-start"]);
    assert!(
        fs::symlink_metadata(account.path(".bashrc"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(&dotfile).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let managed = fs::read_to_string(&dotfile).unwrap();
    fs::write(&dotfile, format!("{managed}# appended by user\n")).unwrap();
    account.success(&["uninstall"]);
    assert_eq!(
        fs::read_to_string(&dotfile).unwrap(),
        "# dotfile\n# appended by user\n"
    );
    assert!(
        fs::symlink_metadata(account.path(".bashrc"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn path_conflicts_retain_installation_and_old_records_can_be_upgraded() {
    let account = Account::new();
    account.success(&["install", "--no-start"]);
    let profile = account.path(".profile");
    let managed = fs::read_to_string(&profile).unwrap();
    fs::write(
        &profile,
        managed.replace("export PATH=", "export CUSTOM_PATH="),
    )
    .unwrap();
    let result = account.run(&["install", "--no-start"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("installation is retained"));
    assert!(!account.run(&["uninstall"]).status.success());
    assert!(account.path(".local/bin/linkrd").exists());
    fs::write(&profile, managed).unwrap();
    account.success(&["uninstall"]);

    account.success(&["install", "--no-start"]);
    let mut manifest = account.manifest();
    manifest.as_object_mut().unwrap().remove("command_path");
    for file in [
        ".profile",
        ".bashrc",
        ".zprofile",
        ".zshrc",
        "config/fish/conf.d/linkrd.fish",
    ] {
        fs::write(account.path(file), "").unwrap();
    }
    fs::write(
        account.path("config/linkrd/installation.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    account.success(&["install", "--no-start"]);
    assert!(
        fs::read_to_string(&profile)
            .unwrap()
            .contains("# >>> linkrd PATH >>>")
    );
    account.success(&["uninstall"]);
}

#[test]
fn partially_written_path_setup_can_be_retried_and_uninstalled() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return;
    }
    for retry in [false, true] {
        let account = Account::new();
        let fish = account.path("config/fish");
        fs::create_dir_all(&fish).unwrap();
        fs::set_permissions(&fish, fs::Permissions::from_mode(0o555)).unwrap();
        let result = account.run(&["install", "--no-start"]);
        fs::set_permissions(&fish, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("installation is retained"));
        assert!(
            fs::read_to_string(account.path(".bashrc"))
                .unwrap()
                .contains("# >>> linkrd PATH >>>")
        );
        if retry {
            account.success(&["install", "--no-start"]);
            assert!(account.path("config/fish/conf.d/linkrd.fish").is_file());
        }
        account.success(&["uninstall"]);
        assert_eq!(fs::read_to_string(account.path(".bashrc")).unwrap(), "");
        assert!(!account.path(".local/bin/linkrd").exists());
    }
}

#[test]
fn registration_failure_retains_installation_and_can_be_retried() {
    let account = Account::new();
    fs::create_dir_all(account.path("config")).unwrap();
    fs::write(account.path("config/autostart"), "obstruction").unwrap();
    let result = account.run(&["install", "--autostart"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("installation is retained"));
    assert!(account.path(".local/bin/linkrd").is_file());
    account.manifest();
    fs::remove_file(account.path("config/autostart")).unwrap();
    account.success(&["autostart", "enable"]);
    account.success(&["uninstall"]);
}

#[test]
fn conflicts_invalid_arguments_and_modified_files_are_not_overwritten_or_deleted() {
    let account = Account::new();
    fs::create_dir_all(account.path(".local/bin")).unwrap();
    let exe = account.path(".local/bin/linkrd");
    fs::write(&exe, "someone else's executable").unwrap();
    assert!(!account.run(&["install", "--no-start"]).status.success());
    assert_eq!(
        fs::read_to_string(&exe).unwrap(),
        "someone else's executable"
    );
    fs::remove_file(&exe).unwrap();
    assert!(
        !account
            .run(&["install", "--", "--startup"])
            .status
            .success()
    );
    assert!(!exe.exists());
    account.success(&["install", "--no-start"]);
    fs::create_dir_all(account.path("config/autostart")).unwrap();
    let entry = account.path("config/autostart/linkrd.desktop");
    fs::write(&entry, "unrelated entry").unwrap();
    assert!(!account.run(&["autostart", "enable"]).status.success());
    assert!(!account.run(&["uninstall"]).status.success());
    assert!(exe.exists());
    assert_eq!(fs::read_to_string(&entry).unwrap(), "unrelated entry");
    fs::remove_file(entry).unwrap();
    fs::write(&exe, "externally replaced executable").unwrap();
    assert!(!account.run(&["install", "--no-start"]).status.success());
    assert!(!account.run(&["uninstall"]).status.success());
    assert_eq!(
        fs::read_to_string(&exe).unwrap(),
        "externally replaced executable"
    );
}

#[test]
fn runtime_lock_blocks_uninstall_and_update_until_process_exits() {
    let account = Account::new();
    account.success(&["install", "--no-start"]);
    let guard = linkrd::install::lock(&account.path("config/linkrd"), "runtime.lock").unwrap();
    for command in ["install", "uninstall"] {
        let result = account.run(&[command]);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("already running"));
    }
    drop(guard);
    account.success(&["uninstall"]);
}

#[test]
fn broken_startup_configuration_is_logged_in_default_directory() {
    let account = Account::new();
    account.success(&["install", "--no-start"]);
    fs::write(
        account.path("config/linkrd/installation.json"),
        "invalid json",
    )
    .unwrap();
    let result = account
        .command(&account.path(".local/bin/linkrd"))
        .arg("--startup")
        .output()
        .unwrap();
    assert!(!result.status.success());
    let log = fs::read_to_string(account.path("data/linkrd/logs/linkrd.log")).unwrap();
    assert!(log.contains("linkrd login startup failed"));
    assert!(log.contains("invalid installation record"));
}

struct BackgroundProcess {
    pid: rustix::process::Pid,
    config: PathBuf,
}

impl Drop for BackgroundProcess {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.pid, rustix::process::Signal::TERM);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if linkrd::install::lock(&self.config, "runtime.lock").is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = rustix::process::kill_process(self.pid, rustix::process::Signal::KILL);
    }
}

#[test]
fn installation_starts_detached_process_with_saved_arguments_and_no_duplicate() {
    for autostart in [false, true] {
        let account = Account::new();
        let mut args = vec!["install"];
        if autostart {
            args.push("--autostart");
        }
        args.extend(["--", "--log-dir", "background logs", "--log-level", "debug"]);
        let output = account.success(&args);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let pid: i32 = stdout
            .split("pid=")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let process = BackgroundProcess {
            pid: rustix::process::Pid::from_raw(pid).unwrap(),
            config: account.path("config/linkrd"),
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        let log_path = account.path("background logs/linkrd.log");
        loop {
            let log = fs::read_to_string(&log_path).unwrap_or_default();
            if log.contains("desktop initialization failed") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "background startup did not reach retry loop: {log}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(linkrd::install::lock(&process.config, "runtime.lock").is_err());
        let reported = account.success(&["pid"]);
        assert_eq!(
            String::from_utf8(reported.stdout).unwrap(),
            format!("{pid}\n")
        );
        assert_eq!(
            String::from_utf8(account.success(&["status"]).stdout).unwrap(),
            format!("running: pid={pid}\n")
        );
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        assert_eq!(fields[3], pid.to_string());
        assert_eq!(
            account.path("config/autostart/linkrd.desktop").exists(),
            autostart
        );
        let again = account.run(&["install"]);
        assert!(!again.status.success());
        assert!(String::from_utf8_lossy(&again.stderr).contains("already running"));
        drop(process);
        assert!(!account.path("config/linkrd/runtime.pid").exists());
        assert_eq!(account.success(&["status"]).stdout, b"stopped\n");
        let stopped = account.run(&["pid"]);
        assert!(!stopped.status.success());
        assert!(stopped.stdout.is_empty());
        account.success(&["uninstall"]);
    }
}

#[test]
fn background_spawn_and_early_exit_failures_are_reported() {
    let account = Account::new();
    let missing = linkrd::install::start_background(&account.path("missing")).unwrap_err();
    assert!(format!("{missing:#}").contains("installation retained"));
    let exited = linkrd::install::start_background(Path::new("/bin/false")).unwrap_err();
    assert!(format!("{exited:#}").contains("background process exited"));
}

#[test]
fn pid_query_handles_missing_stale_and_unavailable_records() {
    let account = Account::new();
    assert_eq!(account.success(&["status"]).stdout, b"stopped\n");
    assert!(!account.run(&["pid"]).status.success());
    let config = account.path("config/linkrd");
    fs::write(config.join("runtime.pid"), std::process::id().to_string()).unwrap();
    assert!(!account.run(&["pid"]).status.success());
    assert_eq!(account.success(&["status"]).stdout, b"stopped\n");
    fs::remove_file(config.join("runtime.pid")).unwrap();
    let old_instance = linkrd::install::lock(&config, "runtime.lock").unwrap();
    let missing = account.run(&["pid"]);
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("PID is unavailable"));
    drop(old_instance);
    fs::write(config.join("runtime.pid"), "0\n").unwrap();
    let instance = linkrd::install::acquire_runtime(&config).unwrap();
    assert_eq!(
        account.success(&["pid"]).stdout,
        format!("{}\n", std::process::id()).as_bytes()
    );
    drop(instance);
    assert!(!config.join("runtime.pid").exists());
}

#[test]
fn forced_exit_does_not_leave_a_running_pid_in_status() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let account = Account::new();
    account.success(&["install", "--no-start"]);
    let mut child = ChildGuard(
        account
            .command(&account.path(".local/bin/linkrd"))
            .arg("--startup")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if account.run(&["pid"]).status.success() {
            break;
        }
        assert!(Instant::now() < deadline, "child did not publish PID");
        std::thread::sleep(Duration::from_millis(25));
    }
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(account.path("config/linkrd/runtime.pid").exists());
    assert_eq!(account.success(&["status"]).stdout, b"stopped\n");
    let result = account.run(&["pid"]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    account.success(&["uninstall"]);
}

#[test]
fn manual_start_stop_and_restart_preserve_autostart_and_settings() {
    let account = Account::new();
    assert_eq!(account.success(&["stop"]).stdout, b"already stopped\n");
    let not_installed = account.run(&["start"]);
    assert!(!not_installed.status.success());
    assert!(String::from_utf8_lossy(&not_installed.stderr).contains("not installed"));
    account.success(&[
        "install",
        "--no-start",
        "--autostart",
        "--",
        "--log-dir",
        "manual logs",
    ]);
    let manifest = account.manifest();
    for _ in 0..2 {
        account.success(&["start"]);
        let pid = String::from_utf8(account.success(&["pid"]).stdout)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let process = BackgroundProcess {
            pid: rustix::process::Pid::from_raw(pid).unwrap(),
            config: account.path("config/linkrd"),
        };
        assert_eq!(
            account.success(&["start"]).stdout,
            format!("already running: pid={pid}\n").as_bytes()
        );
        assert_eq!(
            account.success(&["stop", "--timeout", "3"]).stdout,
            format!("stopped: pid={pid}\n").as_bytes()
        );
        drop(process);
        assert_eq!(account.success(&["status"]).stdout, b"stopped\n");
        assert_eq!(account.success(&["stop"]).stdout, b"already stopped\n");
        assert!(account.path("config/autostart/linkrd.desktop").exists());
        assert_eq!(account.manifest(), manifest);
    }
    assert!(
        fs::read_to_string(account.path("manual logs/linkrd.log"))
            .unwrap()
            .contains("stop requested by local command")
    );
    assert!(!account.run(&["stop", "--timeout", "0"]).status.success());
    assert!(!account.run(&["stop", "--timeout", "301"]).status.success());
    account.success(&["uninstall"]);
}
