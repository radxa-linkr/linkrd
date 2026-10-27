use std::{fs, process::Command};

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_linkrd"))
}

#[test]
fn cli_writes_logs_flushes_on_exit_and_can_disable_files() {
    let root = tempfile::tempdir().unwrap();
    let logs = root.path().join("logs");
    let output = command()
        .args(["--list-ports", "--log-dir"])
        .arg(&logs)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("linkrd starting"));
    assert!(stderr.contains("linkrd stopped"));
    let files: Vec<_> = fs::read_dir(&logs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    let file = fs::read_to_string(&files[0]).unwrap();
    assert!(file.contains("linkrd starting"));
    assert!(file.contains("linkrd stopped"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("linkrd starting"));

    let output = command()
        .args(["--list-ports", "--no-file-log", "--log-level", "error"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("linkrd starting"));
    let unchanged = fs::read_to_string(&files[0]).unwrap();
    assert_eq!(file, unchanged);
}

#[test]
fn unusable_log_directory_falls_back_to_stderr_and_invalid_options_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("ordinary-file");
    fs::write(&file, b"do not replace").unwrap();
    let output = command()
        .args(["--list-ports", "--log-dir"])
        .arg(&file)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("continuing with stderr logging only"));
    assert!(stderr.contains("linkrd starting"));
    assert_eq!(fs::read(&file).unwrap(), b"do not replace");
    for args in [
        vec!["--log-level", "invalid"],
        vec!["--no-file-log", "--log-dir", "unused"],
    ] {
        assert!(!command().args(args).output().unwrap().status.success());
    }
}
