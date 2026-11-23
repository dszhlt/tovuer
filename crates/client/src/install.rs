//! The `tovuer` terminal command. `tovuer --install` (or `/install` in the
//! client) copies the program into a user folder on the PATH, so from then
//! on it starts by name. No administrator rights needed.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context;

/// File name of the command.
pub const FILE_NAME: &str = if cfg!(windows) { "tovuer.exe" } else { "tovuer" };

/// Marker in shell profiles so the PATH line is added only once.
#[cfg(not(windows))]
const MARK: &str = "# tovuer: the tovuer terminal command";

/// What the installation did.
#[derive(Debug)]
pub struct Installed {
    /// Where the program now lives.
    pub path: PathBuf,
    /// The installed copy itself is running — nothing to copy.
    pub already: bool,
    /// Where the folder was put on the PATH, if it was not there yet. Not
    /// empty — the command works in a new terminal window.
    pub path_via: Vec<String>,
}

impl Installed {
    /// What to tell the person, line by line.
    pub fn lines(&self) -> Vec<String> {
        let what = if self.already { "tovuer is already installed" } else { "tovuer installed" };
        let mut out = vec![format!("{what}: {}", self.path.display())];
        if self.path_via.is_empty() {
            out.push("now just type in a terminal: tovuer".into());
        } else {
            let dir = self.path.parent().unwrap_or(Path::new(".")).display().to_string();
            out.push(format!("{dir} was added to PATH ({})", self.path_via.join(", ")));
            out.push("open a new terminal window and type: tovuer".into());
        }
        out
    }
}

/// Where it goes: Linux — `~/.local/bin`, Windows — `%LOCALAPPDATA%\Programs\tovuer`.
pub fn bin_dir(home: &Path) -> PathBuf {
    if cfg!(windows) {
        dirs::data_local_dir().unwrap_or_else(|| home.join("AppData").join("Local")).join("Programs").join("tovuer")
    } else {
        home.join(".local").join("bin")
    }
}

/// Install the running program as the `tovuer` command.
pub fn install() -> anyhow::Result<Installed> {
    let home = dirs::home_dir().context("cannot find the home folder")?;
    let exe = std::env::current_exe().context("cannot tell where the program itself is")?;
    let shell = std::env::var("SHELL").unwrap_or_default();
    install_to(&exe, &bin_dir(&home), &home, std::env::var_os("PATH").as_deref(), &shell)
}

fn install_to(exe: &Path, dir: &Path, home: &Path, path_var: Option<&OsStr>, shell: &str) -> anyhow::Result<Installed> {
    let path = dir.join(FILE_NAME);
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let already = std::fs::canonicalize(&path).is_ok_and(|p| p == exe);
    if !already {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let data = std::fs::read(&exe).context("cannot read the program itself")?;
        crate::transfer::put_binary(&path, &data).with_context(|| format!("cannot write {}", path.display()))?;
    }
    let path_via = if in_path(dir, path_var) { Vec::new() } else { add_to_path(dir, home, shell)? };
    Ok(Installed { path, already, path_via })
}

/// Is the folder on the PATH.
fn in_path(dir: &Path, path_var: Option<&OsStr>) -> bool {
    let want = comparable(dir);
    path_var.is_some_and(|v| std::env::split_paths(v).any(|p| comparable(&p) == want))
}

fn comparable(path: &Path) -> String {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    if cfg!(windows) { text.to_lowercase() } else { text }
}

/// Put the folder on the PATH through shell profiles: the line goes into the
/// existing `~/.bashrc`, `~/.zshrc` and `~/.profile`, and the current shell's
/// file is created if missing.
#[cfg(not(windows))]
fn add_to_path(dir: &Path, home: &Path, shell: &str) -> anyhow::Result<Vec<String>> {
    let shown = match dir.strip_prefix(home) {
        Ok(rel) => format!("$HOME/{}", rel.display()),
        Err(_) => dir.display().to_string(),
    };
    let shell = Path::new(shell).file_name().and_then(OsStr::to_str).unwrap_or("");
    let mut via = Vec::new();
    for (name, current) in [(".bashrc", shell == "bash"), (".zshrc", shell == "zsh"), (".profile", false)] {
        let file = home.join(name);
        if file.exists() || current {
            append_once(&file, &format!("export PATH=\"{shown}:$PATH\""))?;
            via.push(format!("~/{name}"));
        }
    }
    if shell == "fish" || home.join(".config/fish").is_dir() {
        let file = home.join(".config/fish/conf.d/tovuer.fish");
        std::fs::create_dir_all(file.parent().unwrap_or(home))?;
        let body = format!("{MARK}\ncontains \"{shown}\" $PATH; or set -gx PATH \"{shown}\" $PATH\n");
        std::fs::write(&file, body).with_context(|| format!("cannot write {}", file.display()))?;
        via.push("~/.config/fish/conf.d/tovuer.fish".into());
    }
    if via.is_empty() {
        // an unknown shell and no profiles: every sh-compatible shell reads ~/.profile
        append_once(&home.join(".profile"), &format!("export PATH=\"{shown}:$PATH\""))?;
        via.push("~/.profile".into());
    }
    Ok(via)
}

#[cfg(not(windows))]
fn append_once(file: &Path, line: &str) -> anyhow::Result<()> {
    let current = std::fs::read_to_string(file).unwrap_or_default();
    if current.contains(MARK) {
        return Ok(());
    }
    let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
    let text = format!("{current}{sep}\n{MARK}\n{line}\n");
    std::fs::write(file, text).with_context(|| format!("cannot write {}", file.display()))
}

/// Windows: add the folder to the user PATH (registry HKCU\Environment).
/// The value is read without expanding `%variables%` so other entries stay
/// intact; `setx` will not do — it truncates a long PATH. The temporary
/// variable at the end makes Windows tell windows the environment changed.
#[cfg(windows)]
fn add_to_path(dir: &Path, _home: &Path, _shell: &str) -> anyhow::Result<Vec<String>> {
    use std::process::{Command, Stdio};
    const SCRIPT: &str = r#"
$d = $env:TOVUER_BIN
$k = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
$p = [string]$k.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
$parts = @($p -split ';' | Where-Object { $_ })
if ($parts -notcontains $d) {
  $k.SetValue('Path', (($parts + $d) -join ';'), 'ExpandString')
  [Environment]::SetEnvironmentVariable('TOVUER_REFRESH', '1', 'User')
  [Environment]::SetEnvironmentVariable('TOVUER_REFRESH', $null, 'User')
}
"#;
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("TOVUER_BIN", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("cannot start PowerShell to update PATH")?;
    anyhow::ensure!(status.success(), "PowerShell could not add {} to PATH", dir.display());
    Ok(vec!["user PATH".into()])
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tovuer-install-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_itself_and_adds_path_once() {
        let home = temp("path");
        let exe = home.join("tovuer-friend");
        std::fs::write(&exe, b"\x7fELF program").unwrap();
        std::fs::write(home.join(".zshrc"), "alias ll='ls -l'").unwrap();
        let dir = bin_dir(&home);

        let done = install_to(&exe, &dir, &home, Some(OsStr::new("/usr/bin")), "/bin/bash").unwrap();
        assert_eq!(done.path, dir.join("tovuer"));
        assert!(!done.already);
        assert_eq!(std::fs::read(&done.path).unwrap(), b"\x7fELF program");
        assert_eq!(std::fs::metadata(&done.path).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(done.path_via, vec!["~/.bashrc", "~/.zshrc"], "bashrc made for the current shell, zshrc was there");
        let zshrc = std::fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(zshrc.starts_with("alias ll='ls -l'\n") && zshrc.ends_with("export PATH=\"$HOME/.local/bin:$PATH\"\n"), "{zshrc}");
        assert!(!home.join(".profile").exists());

        // installing again does not duplicate the line
        install_to(&exe, &dir, &home, Some(OsStr::new("/usr/bin")), "/bin/bash").unwrap();
        let bashrc = std::fs::read_to_string(home.join(".bashrc")).unwrap();
        assert_eq!(bashrc.matches(MARK).count(), 1, "{bashrc}");

        // running the installed copy with the folder on PATH — nothing is touched
        let path_var = std::env::join_paths([PathBuf::from("/usr/bin"), dir.clone()]).unwrap();
        let again = install_to(&done.path, &dir, &home, Some(&path_var), "/bin/bash").unwrap();
        assert!(again.already && again.path_via.is_empty());
        assert!(again.lines().last().unwrap().contains("just type"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn fish_and_unknown_shells() {
        let home = temp("fish");
        let exe = home.join("tovuer");
        std::fs::write(&exe, b"x").unwrap();
        let done = install_to(&exe, &bin_dir(&home), &home, None, "/usr/bin/fish").unwrap();
        assert_eq!(done.path_via, vec!["~/.config/fish/conf.d/tovuer.fish"]);
        let fish = std::fs::read_to_string(home.join(".config/fish/conf.d/tovuer.fish")).unwrap();
        assert!(fish.contains("set -gx PATH \"$HOME/.local/bin\" $PATH"));
        std::fs::remove_dir_all(&home).unwrap();

        let home = temp("sh");
        let exe = home.join("tovuer");
        std::fs::write(&exe, b"x").unwrap();
        let done = install_to(&exe, &bin_dir(&home), &home, None, "").unwrap();
        assert_eq!(done.path_via, vec!["~/.profile"]);
        std::fs::remove_dir_all(home).unwrap();
    }
}
