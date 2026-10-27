use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct Configuration {
    #[serde(default)]
    profiles: Vec<Profile>,
    #[serde(default)]
    windows_path_added: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Profile {
    file: PathBuf,
    block: String,
}

fn directory_text(directory: &Path) -> Result<&str> {
    let text = directory
        .to_str()
        .context("PATH directory must be valid Unicode")?;
    ensure!(
        directory.is_absolute() && !text.chars().any(char::is_control),
        "PATH directory must be absolute and cannot contain control characters"
    );
    ensure!(
        !text.contains(if cfg!(windows) { ';' } else { ':' }),
        "installation directory contains a PATH separator; choose another directory"
    );
    Ok(text)
}

#[cfg(not(windows))]
const BEGIN: &str = "# >>> linkrd PATH >>>";
#[cfg(not(windows))]
const END: &str = "# <<< linkrd PATH <<<";

#[cfg(not(windows))]
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(not(windows))]
fn fish_quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(not(windows))]
fn profile_block(directory: &Path, fish: bool) -> Result<String> {
    let text = directory_text(directory)?;
    let body = if fish {
        let quoted = fish_quote(text);
        format!("if not contains -- {quoted} $PATH\n    set -gx PATH {quoted} $PATH\nend\n")
    } else {
        let quoted = shell_quote(text);
        format!(
            "case \":${{PATH-}}:\" in\n    *:{quoted}:*) ;;\n    *) export PATH={quoted}${{PATH:+\":$PATH\"}} ;;\nesac\n"
        )
    };
    Ok(format!("\n{BEGIN}\n{body}{END}\n"))
}

#[cfg(not(windows))]
fn read_profile(file: &Path) -> Result<String> {
    match std::fs::read_to_string(file) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", file.display())),
    }
}

#[cfg(not(windows))]
fn check_block(content: &str, profile: &Profile) -> Result<bool> {
    let has_block = content.matches(&profile.block).count() == 1;
    let begins = content.lines().filter(|line| *line == BEGIN).count();
    let ends = content.lines().filter(|line| *line == END).count();
    ensure!(
        (begins == 0 && ends == 0) || (has_block && begins == 1 && ends == 1),
        "linkrd PATH block in {} was modified; inspect it before removing it manually",
        profile.file.display()
    );
    Ok(has_block)
}

#[cfg(not(windows))]
fn write_profile(file: &Path, content: &str) -> Result<()> {
    use std::io::Write;
    let target = match std::fs::symlink_metadata(file) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::canonicalize(file)?,
        Ok(_) => file.to_path_buf(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => file.to_path_buf(),
        Err(error) => return Err(error.into()),
    };
    let file = target.as_path();
    let parent = file
        .parent()
        .context("shell profile has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    match std::fs::metadata(file) {
        Ok(metadata) => staged.as_file().set_permissions(metadata.permissions())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    staged.write_all(content.as_bytes())?;
    staged.as_file().sync_all()?;
    staged.persist(file).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn plan(directory: &Path, previous: &Configuration) -> Result<Configuration> {
    directory_text(directory)?;
    let home = dirs::home_dir().context("cannot locate home directory")?;
    let zsh = std::env::var_os("ZDOTDIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.clone());
    let fish = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    ensure!(
        zsh.is_absolute() && fish.is_absolute(),
        "shell configuration directories must be absolute"
    );
    let mut targets = vec![
        (home.join(".profile"), false),
        (home.join(".bashrc"), false),
        (zsh.join(".zprofile"), false),
        (zsh.join(".zshrc"), false),
        (fish.join("fish/conf.d/linkrd.fish"), true),
    ];
    for name in [".bash_profile", ".bash_login"] {
        let file = home.join(name);
        if file.try_exists()? {
            targets.push((file, false));
            break;
        }
    }
    let mut configuration = previous.clone();
    for (file, fish) in targets {
        let file = if std::fs::symlink_metadata(&file).is_ok() {
            std::fs::canonicalize(&file)?
        } else {
            file
        };
        if !configuration
            .profiles
            .iter()
            .any(|profile| profile.file == file)
        {
            configuration.profiles.push(Profile {
                file,
                block: profile_block(directory, fish)?,
            });
        }
    }
    for profile in &configuration.profiles {
        check_block(&read_profile(&profile.file)?, profile)?;
    }
    Ok(configuration)
}

#[cfg(not(windows))]
pub(super) fn enable(_directory: &Path, configuration: &Configuration) -> Result<()> {
    for profile in &configuration.profiles {
        let mut content = read_profile(&profile.file)?;
        if !check_block(&content, profile)? {
            content.push_str(&profile.block);
            write_profile(&profile.file, &content)?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn disable(_directory: &Path, configuration: &Configuration) -> Result<()> {
    let updates = configuration
        .profiles
        .iter()
        .map(|profile| {
            let content = read_profile(&profile.file)?;
            let updated = check_block(&content, profile)?.then(|| {
                let (before, after) = content.split_once(&profile.block).unwrap();
                let separator = if !before.is_empty()
                    && !before.ends_with('\n')
                    && !after.is_empty()
                    && !after.starts_with('\n')
                {
                    "\n"
                } else {
                    ""
                };
                format!("{before}{separator}{after}")
            });
            Ok((&profile.file, updated))
        })
        .collect::<Result<Vec<_>>>()?;
    for (file, updated) in updates {
        if let Some(content) = updated {
            write_profile(file, &content)?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn print_instructions(directory: &Path) -> Result<()> {
    let text = directory_text(directory)?;
    println!("Command PATH configured for this user; open a new terminal to run linkrd.");
    if std::env::var_os("SHELL").is_some_and(|shell| {
        Path::new(&shell)
            .file_name()
            .is_some_and(|name| name == "fish")
    }) {
        println!(
            "For this fish session: set -gx PATH {} $PATH",
            fish_quote(text)
        );
    } else {
        println!(
            "For this terminal (sh/bash/zsh): export PATH={}${{PATH:+\":$PATH\"}}",
            shell_quote(text)
        );
    }
    Ok(())
}

#[cfg(windows)]
fn read_user_path() -> Result<Option<windows_registry::Value>> {
    match windows_registry::CURRENT_USER
        .open("Environment")
        .and_then(|key| key.get_value("Path"))
    {
        Ok(value) => {
            ensure!(
                matches!(
                    value.ty(),
                    windows_registry::Type::String | windows_registry::Type::ExpandString
                ),
                "user PATH registry value is not a string"
            );
            Ok(Some(value))
        }
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
fn expand(text: &str) -> Result<String> {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    let source: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let length = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), std::ptr::null_mut(), 0) };
    ensure!(length > 0, "cannot expand user PATH");
    let mut result = vec![0; length as usize];
    let written =
        unsafe { ExpandEnvironmentStringsW(source.as_ptr(), result.as_mut_ptr(), length) };
    ensure!(written > 0 && written <= length, "cannot expand user PATH");
    Ok(String::from_utf16(&result[..written as usize - 1])?)
}

#[cfg(any(windows, test))]
fn windows_path_text(text: &str) -> String {
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
    }
}

#[cfg(windows)]
fn windows_directory(directory: &Path) -> Result<String> {
    Ok(windows_path_text(directory_text(directory)?))
}

#[cfg(any(windows, test))]
fn matches_directory(entry: &str, directory: &str) -> bool {
    let normalize = |text: &str| {
        windows_path_text(text.trim().trim_matches('"'))
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    normalize(entry) == normalize(directory)
}

#[cfg(windows)]
pub(super) fn plan(directory: &Path, previous: &Configuration) -> Result<Configuration> {
    let directory = windows_directory(directory)?;
    let value = read_user_path()?;
    let mut configuration = previous.clone();
    let text = value
        .clone()
        .map(String::try_from)
        .transpose()?
        .unwrap_or_default();
    let expanded = if value
        .as_ref()
        .is_some_and(|v| v.ty() == windows_registry::Type::ExpandString)
    {
        expand(&text)?
    } else {
        text
    };
    if !expanded
        .split(';')
        .any(|entry| matches_directory(entry, &directory))
    {
        configuration.windows_path_added = true;
    }
    Ok(configuration)
}

#[cfg(windows)]
fn write_user_path(text: &str, ty: windows_registry::Type) -> Result<()> {
    let key = windows_registry::CURRENT_USER.create("Environment")?;
    let mut value = windows_registry::Value::from(text);
    value.set_ty(ty);
    key.set_value("Path", &value)?;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let environment: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            1000,
            std::ptr::null_mut(),
        );
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn enable(directory: &Path, configuration: &Configuration) -> Result<()> {
    if !configuration.windows_path_added {
        return Ok(());
    }
    let directory = windows_directory(directory)?;
    let value = read_user_path()?;
    let ty = value
        .as_ref()
        .map_or(windows_registry::Type::String, |v| v.ty());
    let mut text = value.map(String::try_from).transpose()?.unwrap_or_default();
    let expanded = if ty == windows_registry::Type::ExpandString {
        expand(&text)?
    } else {
        text.clone()
    };
    if expanded
        .split(';')
        .any(|entry| matches_directory(entry, &directory))
    {
        return Ok(());
    }
    ensure!(
        ty != windows_registry::Type::ExpandString || !directory.contains('%'),
        "installation directory contains '%' and cannot be added to an expandable PATH"
    );
    if !text.is_empty() && !text.ends_with(';') {
        text.push(';');
    }
    text.push_str(&directory);
    write_user_path(&text, ty)
}

#[cfg(windows)]
pub(super) fn disable(directory: &Path, configuration: &Configuration) -> Result<()> {
    if !configuration.windows_path_added {
        return Ok(());
    }
    let directory = windows_directory(directory)?;
    if let Some(value) = read_user_path()? {
        let ty = value.ty();
        let text = String::try_from(value)?;
        let mut entries: Vec<_> = text.split(';').collect();
        if let Some(index) = entries
            .iter()
            .position(|entry| matches_directory(entry, &directory))
        {
            entries.remove(index);
            write_user_path(&entries.join(";"), ty)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn print_instructions(directory: &Path) -> Result<()> {
    let directory = windows_directory(directory)?.replace('\'', "''");
    println!(
        "User PATH configured; restart your terminal application (or sign out and back in) to run linkrd."
    );
    println!("For this PowerShell session: $env:Path = '{directory};' + $env:Path");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_path_handles_canonical_paths_and_existing_equivalent_entries() {
        assert_eq!(
            windows_path_text(r"\\?\C:\Users\测试 a\linkrd"),
            r"C:\Users\测试 a\linkrd"
        );
        assert_eq!(
            windows_path_text(r"\\?\UNC\server\share\linkrd"),
            r"\\server\share\linkrd"
        );
        assert!(matches_directory(
            r#" "c:/Users/Test/linkrd/" "#,
            r"\\?\C:\Users\Test\linkrd"
        ));
        assert!(!matches_directory(
            r"C:\Users\Test\linkrd-other",
            r"C:\Users\Test\linkrd"
        ));
    }
}
