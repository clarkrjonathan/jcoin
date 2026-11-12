//! Installing the app for the current user.
//!
//! The download is a single program file with a long name
//! (`JeffyCoin-windows-x86_64.exe`) that runs from wherever it was saved.
//! Installing copies it to one fixed place under a short name and adds an
//! entry to the Start menu (Windows) or the application menu (Linux), so the
//! app is found by searching for "JeffyCoin" and an update always replaces the
//! same file. The download is left where it is. No administrator rights are
//! needed.
//!
//! | | Windows | Linux |
//! | --- | --- | --- |
//! | Program | `%LOCALAPPDATA%\Programs\JeffyCoin\JeffyCoin.exe` | `~/.local/bin/jeffycoin` |
//! | Menu entry | `Start Menu\Programs\JeffyCoin.lnk` | `~/.local/share/applications/jeffycoin.desktop` |
//! | Installed version | `version.txt` next to the program | `~/.local/share/jeffycoin-install/version.txt` |

use std::path::{Path, PathBuf};
use std::process::Command;

/// The name shown in the Start menu and the application menu.
pub const APP_NAME: &str = "JeffyCoin";

/// The menu icon (Linux; a Windows shortcut shows the program's own icon).
const ICON_SVG: &[u8] = include_bytes!("../assets/jeffycoin.svg");

/// Where an install puts its files.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// The installed program.
    pub program: PathBuf,
    /// The Start menu shortcut or `.desktop` file.
    pub menu_entry: PathBuf,
    /// The menu icon file, where the system uses one.
    pub icon: Option<PathBuf>,
    /// A text file holding the installed version.
    pub version_file: PathBuf,
}

impl Layout {
    /// The layout for the current user, or `None` if the system's folders
    /// cannot be found or the system is not supported.
    pub fn for_user() -> Option<Layout> {
        // An empty variable counts as unset.
        let env = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        if cfg!(windows) {
            let dir = env("LOCALAPPDATA")?.join("Programs").join(APP_NAME);
            Some(Layout {
                program: dir.join("JeffyCoin.exe"),
                menu_entry: env("APPDATA")?
                    .join("Microsoft/Windows/Start Menu/Programs")
                    .join("JeffyCoin.lnk"),
                icon: None,
                version_file: dir.join("version.txt"),
            })
        } else if cfg!(target_os = "linux") {
            let home = env("HOME")?;
            let data = env("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
            Some(Layout {
                program: home.join(".local/bin/jeffycoin"),
                menu_entry: data.join("applications/jeffycoin.desktop"),
                icon: Some(data.join("icons/hicolor/scalable/apps/jeffycoin.svg")),
                version_file: data.join("jeffycoin-install/version.txt"),
            })
        } else {
            None
        }
    }

    /// The installed version, if the app is installed.
    pub fn installed_version(&self) -> Option<String> {
        if !self.program.exists() {
            return None;
        }
        let text = std::fs::read_to_string(&self.version_file).unwrap_or_default();
        let version = text.trim();
        Some(if version.is_empty() { "unknown".to_string() } else { version.to_string() })
    }

    /// True if `exe` is the installed program.
    pub fn is_installed_copy(&self, exe: &Path) -> bool {
        match (std::fs::canonicalize(exe), std::fs::canonicalize(&self.program)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// Copy `source` (a JeffyCoin program file of version `version`) into
    /// place and add the menu entry. An existing install is replaced.
    pub fn install(&self, source: &Path, version: &str) -> Result<(), String> {
        let make_parent = |path: &Path| match path.parent() {
            Some(dir) => std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display())),
            None => Ok(()),
        };
        make_parent(&self.program)?;
        // Copy to a temporary name, then rename over the program: a running
        // installed copy is never half-written.
        let staged = self.program.with_extension("installing");
        std::fs::copy(source, &staged)
            .map_err(|e| format!("cannot copy the program to {}: {e}", staged.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
        }
        if let Err(e) = std::fs::rename(&staged, &self.program) {
            let _ = std::fs::remove_file(&staged);
            return Err(format!(
                "cannot put the program at {} ({e}). If JeffyCoin is open from the installed copy, close it and try again.",
                self.program.display()
            ));
        }
        self.record_version(version);

        if let Some(icon) = &self.icon {
            make_parent(icon)?;
            std::fs::write(icon, ICON_SVG).map_err(|e| format!("cannot write the icon: {e}"))?;
        }
        make_parent(&self.menu_entry)?;
        self.write_menu_entry()?;
        jcoin::log_info!(
            [("event", "installed".to_string()), ("version", version.to_string())];
            "[app] installed version {version}"
        );
        Ok(())
    }

    /// Record `version` as the installed version. The installed copy calls
    /// this at startup, so the record is right after an in-app update too.
    pub fn record_version(&self, version: &str) {
        if let Some(dir) = self.version_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.version_file, version);
    }

    fn write_menu_entry(&self) -> Result<(), String> {
        if cfg!(windows) {
            // A .lnk file can only be made through the Windows shell.
            let script = shortcut_script(&self.menu_entry, &self.program);
            let mut command = Command::new("powershell");
            command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
            hide_console(&mut command);
            let output = command
                .output()
                .map_err(|e| format!("cannot run PowerShell to make the Start menu entry: {e}"))?;
            if !output.status.success() || !self.menu_entry.exists() {
                return Err(format!(
                    "the program is installed, but the Start menu entry could not be made: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            Ok(())
        } else {
            std::fs::write(&self.menu_entry, desktop_entry(&self.program))
                .map_err(|e| format!("cannot write {}: {e}", self.menu_entry.display()))
        }
    }

    /// Remove the menu entry, the icon, the version record, and the program.
    /// The data folder (keys and chain) is not touched. On Windows a running
    /// program cannot delete itself, so if `self.program` is the running
    /// program it is deleted a few seconds after the app exits.
    pub fn uninstall(&self, running: &Path) -> Result<(), String> {
        jcoin::log_info!([("event", "uninstalled".to_string())]; "[app] uninstalling");
        let _ = std::fs::remove_file(&self.menu_entry);
        if let Some(icon) = &self.icon {
            let _ = std::fs::remove_file(icon);
        }
        let _ = std::fs::remove_file(&self.version_file);
        if cfg!(windows) && self.is_installed_copy(running) {
            let mut command = Command::new("cmd");
            command.args([
                "/C",
                &format!("ping -n 4 127.0.0.1 >nul & del /F /Q \"{}\"", self.program.display()),
            ]);
            hide_console(&mut command);
            command
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("cannot schedule removing the program: {e}"))
        } else {
            match std::fs::remove_file(&self.program) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("cannot remove {}: {e}", self.program.display())),
            }
        }
    }
}

/// The `.desktop` file that puts the app in a Linux application menu.
fn desktop_entry(program: &Path) -> String {
    // Exec quoting per the Desktop Entry Specification: the path in double
    // quotes, with `"`, `` ` ``, `$`, and `\` escaped.
    let mut exec = String::new();
    for c in program.display().to_string().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            exec.push('\\');
        }
        exec.push(c);
    }
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={APP_NAME}\n\
         GenericName=Cryptocurrency wallet and miner\n\
         Comment=JeffyCoin node, wallet, and GPU miner\n\
         Exec=\"{exec}\"\n\
         Icon=jeffycoin\n\
         Terminal=false\n\
         Categories=Network;Finance;\n\
         Keywords=jeffy;coin;wallet;miner;crypto;\n\
         StartupWMClass=jcoin-app\n"
    )
}

/// The PowerShell command that makes the Start menu shortcut.
fn shortcut_script(shortcut: &Path, program: &Path) -> String {
    // Single-quoted PowerShell strings: the only escape is '' for '.
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "''"));
    let folder = program.parent().unwrap_or(program);
    format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); \
         $s.TargetPath = {}; $s.WorkingDirectory = {}; \
         $s.Description = 'JeffyCoin node, wallet, and GPU miner'; $s.Save()",
        quote(shortcut),
        quote(program),
        quote(folder),
    )
}

/// Keep a helper command from flashing a console window on Windows.
fn hide_console(_command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        _command.creation_flags(CREATE_NO_WINDOW);
    }
}

/// Start the program at `path` and end this process.
pub fn hand_over_to(path: &Path) -> Result<(), String> {
    Command::new(path)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", path.display()))?;
    crate::telemetry::flush();
    std::process::exit(0);
}

/// Open `folder` in the system's file manager.
pub fn open_folder(folder: &Path) -> Result<(), String> {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(opener)
        .arg(folder)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot open the folder: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_layout(name: &str) -> (PathBuf, Layout) {
        let root = std::env::temp_dir().join(format!("jcoin-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = Layout {
            program: root.join("bin/jeffycoin"),
            menu_entry: root.join("applications/jeffycoin.desktop"),
            icon: Some(root.join("icons/jeffycoin.svg")),
            version_file: root.join("meta/version.txt"),
        };
        (root, layout)
    }

    #[cfg(not(windows))]
    #[test]
    fn install_copies_the_program_and_adds_the_menu_entry() {
        let (root, layout) = temp_layout("full");
        std::fs::create_dir_all(&root).unwrap();
        let download = root.join("JeffyCoin-linux-x86_64");
        std::fs::write(&download, b"program v1").unwrap();
        assert_eq!(layout.installed_version(), None);
        assert!(!layout.is_installed_copy(&download));

        layout.install(&download, "2.1.2").unwrap();
        assert_eq!(std::fs::read(&layout.program).unwrap(), b"program v1");
        assert!(download.exists(), "the download is left in place");
        assert_eq!(layout.installed_version().as_deref(), Some("2.1.2"));
        assert!(layout.is_installed_copy(&layout.program));
        assert!(!layout.is_installed_copy(&download));
        let entry = std::fs::read_to_string(&layout.menu_entry).unwrap();
        assert!(entry.contains("Name=JeffyCoin"));
        assert!(entry.contains(&format!("Exec=\"{}\"", layout.program.display())));
        assert!(layout.icon.as_ref().unwrap().exists());
        // If the system has the freedesktop validator, the entry must pass it.
        if let Ok(out) = Command::new("desktop-file-validate").arg(&layout.menu_entry).output() {
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&layout.program).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "the installed program is executable");
        }

        // Installing a newer file replaces the program and the version.
        std::fs::write(&download, b"program v2").unwrap();
        layout.install(&download, "2.2.0").unwrap();
        assert_eq!(std::fs::read(&layout.program).unwrap(), b"program v2");
        assert_eq!(layout.installed_version().as_deref(), Some("2.2.0"));

        layout.uninstall(&download).unwrap();
        assert!(!layout.program.exists() && !layout.menu_entry.exists());
        assert_eq!(layout.installed_version(), None);
        assert!(download.exists());
        layout.uninstall(&download).unwrap(); // removing twice is not an error
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn menu_entry_text_escapes_awkward_paths() {
        let entry = desktop_entry(Path::new("/home/a b/$x/jeffycoin"));
        assert!(entry.contains("Exec=\"/home/a b/\\$x/jeffycoin\"\n"));
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\nName=JeffyCoin\n"));

        let script = shortcut_script(
            Path::new(r"C:\Users\O'Neil\Start Menu\JeffyCoin.lnk"),
            Path::new(r"C:\Users\O'Neil\Programs\JeffyCoin\JeffyCoin.exe"),
        );
        assert!(script.contains(r"CreateShortcut('C:\Users\O''Neil\Start Menu\JeffyCoin.lnk')"));
        assert!(script.contains(r"$s.TargetPath = 'C:\Users\O''Neil\Programs\JeffyCoin\JeffyCoin.exe'"));
        // Only Windows splits a path at `\`, so only there is the folder known.
        if cfg!(windows) {
            assert!(script.contains(r"$s.WorkingDirectory = 'C:\Users\O''Neil\Programs\JeffyCoin'"));
        }
    }

    #[test]
    fn the_user_layout_uses_a_short_program_name() {
        if let Some(layout) = Layout::for_user() {
            let name = layout.program.file_name().unwrap().to_string_lossy().to_lowercase();
            assert!(name == "jeffycoin" || name == "jeffycoin.exe");
        }
    }
}
