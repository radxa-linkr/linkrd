use anyhow::{Context, Result, ensure};
use std::path::Path;

#[cfg(not(windows))]
use std::{fs, io, path::PathBuf};

fn path_text(path: &Path) -> Result<&str> {
    let text = path
        .to_str()
        .context("autostart executable path must be valid Unicode")?;
    ensure!(
        !text.chars().any(char::is_control),
        "autostart path cannot contain control characters"
    );
    Ok(text)
}

#[cfg(any(target_os = "linux", test))]
fn desktop_entry(path: &Path) -> Result<String> {
    let path = path_text(path)?;
    ensure!(
        !path.contains('='),
        "XDG autostart executable path cannot contain '='"
    );
    let mut quoted = String::from("\"");
    for character in path.chars() {
        match character {
            '\\' => quoted.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(character);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=linkrd\nComment=ACM clipboard agent\nExec={quoted} --startup\nTerminal=false\nStartupNotify=false\nX-linkrd-Managed=true\n"
    ))
}

#[cfg(any(target_os = "macos", test))]
fn launch_agent(path: &Path) -> Result<String> {
    let path = path_text(path)?
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>linkrd</string>
<key>ProgramArguments</key><array><string>{path}</string><string>--startup</string></array>
<key>RunAtLoad</key><true/>
<key>LimitLoadToSessionType</key><string>Aqua</string>
</dict></plist>
"#
    ))
}

#[cfg(any(windows, test))]
fn run_command(path: &Path) -> Result<String> {
    let path = path_text(path)?;
    ensure!(
        !path.contains('"'),
        "Windows executable path cannot contain quotes"
    );
    let command = format!("\"{path}\" --startup");
    ensure!(
        command.encode_utf16().count() <= 260,
        "Windows Run command exceeds 260 characters; choose a shorter installation directory"
    );
    Ok(command)
}

#[cfg(not(windows))]
fn entry(path: &Path) -> Result<(PathBuf, String)> {
    #[cfg(target_os = "linux")]
    return Ok((
        dirs::config_dir()
            .context("cannot locate user configuration directory")?
            .join("autostart/linkrd.desktop"),
        desktop_entry(path)?,
    ));
    #[cfg(target_os = "macos")]
    return Ok((
        dirs::home_dir()
            .context("cannot locate home directory")?
            .join("Library/LaunchAgents/linkrd.plist"),
        launch_agent(path)?,
    ));
}

#[cfg(not(windows))]
fn read_entry(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_file(),
            "autostart entry {} is not a regular file",
            path.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    Ok(Some(fs::read_to_string(path)?))
}

#[cfg(not(windows))]
pub(super) fn enable(executable: &Path) -> Result<()> {
    let (path, expected) = entry(executable)?;
    let actual = read_entry(&path)?;
    ensure!(
        actual.as_ref().is_none_or(|text| text == &expected),
        "autostart entry {} already exists with different content; inspect it before removing it manually",
        path.display()
    );
    super::atomic_write(&path, expected.as_bytes())
}

#[cfg(not(windows))]
pub(super) fn disable(executable: &Path) -> Result<()> {
    let (path, expected) = entry(executable)?;
    if let Some(actual) = read_entry(&path)? {
        ensure!(
            actual == expected,
            "autostart entry {} was modified; inspect it before removing it manually",
            path.display()
        );
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn status(executable: &Path) -> Result<&'static str> {
    let (path, expected) = entry(executable)?;
    Ok(match read_entry(&path)? {
        None => "not registered",
        Some(actual) if actual == expected => "registered",
        Some(_) => "entry differs from managed configuration; inspect it manually",
    })
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
fn read_run() -> Result<Option<String>> {
    match windows_registry::CURRENT_USER
        .open(RUN_KEY)
        .and_then(|key| key.get_string("linkrd"))
    {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error.into())
            }
        }
    }
}

#[cfg(windows)]
pub(super) fn enable(executable: &Path) -> Result<()> {
    let expected = run_command(executable)?;
    ensure!(
        read_run()?.is_none_or(|actual| actual == expected),
        "HKCU Run/linkrd already contains a different command; inspect it before removing it manually"
    );
    windows_registry::CURRENT_USER
        .create(RUN_KEY)?
        .set_string("linkrd", expected)?;
    Ok(())
}

#[cfg(windows)]
pub(super) fn disable(executable: &Path) -> Result<()> {
    if let Some(actual) = read_run()? {
        ensure!(
            actual == run_command(executable)?,
            "HKCU Run/linkrd was modified; inspect it before removing it manually"
        );
        windows_registry::CURRENT_USER
            .options()
            .write()
            .open(RUN_KEY)?
            .remove_value("linkrd")?;
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn status(executable: &Path) -> Result<&'static str> {
    Ok(match read_run()? {
        None => "not registered",
        Some(actual) if actual == run_command(executable)? => "registered",
        Some(_) => "entry differs from managed configuration; inspect HKCU Run/linkrd manually",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_paths_preserve_spaces_unicode_and_metacharacters() {
        let desktop = desktop_entry(Path::new("/home/测试 a/$`\\\"%/linkrd")).unwrap();
        assert!(desktop.contains(r#"Exec="/home/测试 a/\\$\\`\\\\\\"%%/linkrd" --startup"#));
        assert!(desktop_entry(Path::new("/tmp/a=b/linkrd")).is_err());
        assert!(desktop_entry(Path::new("/tmp/a\nb/linkrd")).is_err());
        let plist = launch_agent(Path::new("/Users/a & <b>/linkrd")).unwrap();
        assert!(plist.contains("<string>/Users/a &amp; &lt;b&gt;/linkrd</string>"));
        assert_eq!(
            run_command(Path::new(r"C:\Users\测试 a\linkrd.exe")).unwrap(),
            r#""C:\Users\测试 a\linkrd.exe" --startup"#
        );
        assert!(run_command(Path::new(&"a".repeat(260))).is_err());
    }
}
