//! Integrazione con Esplora file (solo Windows, solo utente corrente: nessun permesso di amministratore).
use std::path::PathBuf;

#[cfg(windows)]
mod imp {
    use super::PathBuf;
    use std::{env, fs, io};
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    const ROOT: &str = r"Software\Classes";

    fn entries(exe: &str) -> Vec<(String, &'static str, String)> {
        vec![
            (format!(r"{ROOT}\*\shell\CopyRs"), "Copia con CopyRs", format!("\"{exe}\" \"%1\"")),
            (format!(r"{ROOT}\Directory\shell\CopyRs"), "Copia con CopyRs", format!("\"{exe}\" \"%1\"")),
            (format!(r"{ROOT}\Directory\shell\CopyRsPaste"), "Incolla con CopyRs", format!("\"{exe}\" --paste \"%1\"")),
            (format!(r"{ROOT}\Directory\Background\shell\CopyRsPaste"), "Incolla con CopyRs", format!("\"{exe}\" --paste \"%V\"")),
        ]
    }

    fn marker() -> Option<PathBuf> {
        Some(PathBuf::from(env::var_os("APPDATA")?).join("CopyRs").join("setup_done"))
    }

    pub fn needs_prompt() -> bool {
        marker().map(|m| !m.exists()).unwrap_or(false)
    }

    pub fn mark_done() {
        if let Some(m) = marker() {
            if let Some(p) = m.parent() {
                let _ = fs::create_dir_all(p);
            }
            let _ = fs::write(m, "1");
        }
    }

    pub fn install() -> io::Result<()> {
        let exe = env::current_exe()?.to_string_lossy().to_string();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for (path, label, cmd) in entries(&exe) {
            let (k, _) = hkcu.create_subkey(&path)?;
            k.set_value("", &label)?;
            k.set_value("Icon", &exe)?;
            let (c, _) = k.create_subkey("command")?;
            c.set_value("", &cmd)?;
        }
        notify();
        let (ok, total) = status();
        if ok != total {
            return Err(io::Error::new(io::ErrorKind::Other, format!("scritte solo {ok} voci su {total}")));
        }
        Ok(())
    }

    pub fn uninstall() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for (path, _, _) in entries("") {
            let _ = hkcu.delete_subkey_all(&path);
        }
        notify();
        Ok(())
    }

    /// Voci presenti nel registro con il comando giusto per questo exe, e numero totale previsto.
    pub fn status() -> (usize, usize) {
        let exe = env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let list = entries(&exe);
        let ok = list
            .iter()
            .filter(|(path, _, cmd)| {
                hkcu.open_subkey(format!(r"{path}\command"))
                    .ok()
                    .and_then(|k| k.get_value::<String, _>("").ok())
                    .map(|v| &v == cmd)
                    .unwrap_or(false)
            })
            .count();
        (ok, list.len())
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(event: i32, flags: u32, item1: *const std::ffi::c_void, item2: *const std::ffi::c_void);
    }

    /// Dice a Esplora file che le associazioni sono cambiate, cosi' rilegge subito il menu.
    fn notify() {
        unsafe { SHChangeNotify(0x0800_0000, 0, std::ptr::null(), std::ptr::null()) }
    }

    fn ps(script: &str) -> io::Result<()> {
        use std::os::windows::process::CommandExt;
        let st = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-Command", script])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()?;
        if st.success() {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "PowerShell ha restituito un errore"))
        }
    }

    /// Crea i collegamenti "CopyRs" e "CopyRs - Impostazioni" (Desktop e menu Start) e la voce "Invia a".
    pub fn create_shortcuts() -> io::Result<()> {
        let exe = env::current_exe()?.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$exe='{exe}'; $ws=New-Object -ComObject WScript.Shell; \
             $desk=[Environment]::GetFolderPath('Desktop'); \
             $start=Join-Path $env:APPDATA 'Microsoft\\Windows\\Start Menu\\Programs'; \
             $send=Join-Path $env:APPDATA 'Microsoft\\Windows\\SendTo'; \
             function Mk($dir,$name,$a){{ $s=$ws.CreateShortcut((Join-Path $dir ($name+'.lnk'))); $s.TargetPath=$exe; $s.Arguments=$a; $s.WorkingDirectory=(Split-Path $exe); $s.IconLocation=$exe; $s.Save() }}; \
             foreach($d in @($desk,$start)){{ Mk $d 'CopyRs' ''; Mk $d 'CopyRs - Impostazioni' '--settings' }}; \
             Mk $send 'CopyRs' ''"
        );
        ps(&script)
    }

    pub fn remove_shortcuts() -> io::Result<()> {
        ps("$desk=[Environment]::GetFolderPath('Desktop'); \
            $start=Join-Path $env:APPDATA 'Microsoft\\Windows\\Start Menu\\Programs'; \
            $send=Join-Path $env:APPDATA 'Microsoft\\Windows\\SendTo'; \
            foreach($d in @($desk,$start)){ Remove-Item -ErrorAction SilentlyContinue (Join-Path $d 'CopyRs.lnk'),(Join-Path $d 'CopyRs - Impostazioni.lnk') }; \
            Remove-Item -ErrorAction SilentlyContinue (Join-Path $send 'CopyRs.lnk'); exit 0")
    }

    /// Apre un indirizzo web nel browser predefinito.
    pub fn open_url(url: &str) {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .creation_flags(0x0800_0000)
            .spawn();
    }

    /// File negli appunti di Esplora file + true se erano stati "tagliati" (Ctrl+X).
    pub fn clipboard_files() -> (Vec<PathBuf>, bool) {
        use clipboard_win::{formats, get_clipboard, register_format};
        let files: Vec<String> = get_clipboard(formats::FileList).unwrap_or_default();
        let cut = register_format("Preferred DropEffect")
            .and_then(|f| get_clipboard::<Vec<u8>, _>(formats::RawData(f.get())).ok())
            .map(|b| b.first() == Some(&2))
            .unwrap_or(false);
        (files.into_iter().map(PathBuf::from).collect(), cut)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::PathBuf;
    use std::io;
    pub fn needs_prompt() -> bool { false }
    pub fn mark_done() {}
    pub fn install() -> io::Result<()> { Err(io::Error::new(io::ErrorKind::Unsupported, "solo Windows")) }
    pub fn uninstall() -> io::Result<()> { Ok(()) }
    pub fn status() -> (usize, usize) { (0, 0) }
    pub fn create_shortcuts() -> io::Result<()> { Err(io::Error::new(io::ErrorKind::Unsupported, "solo Windows")) }
    pub fn remove_shortcuts() -> io::Result<()> { Ok(()) }
    pub fn open_url(url: &str) { let _ = std::process::Command::new("xdg-open").arg(url).spawn(); }
    pub fn clipboard_files() -> (Vec<PathBuf>, bool) { (Vec::new(), false) }
}

pub use imp::*;

pub fn settings_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("CopyRs"))
}

/// Tema salvato: scuro se non c'e' ancora nessuna scelta.
pub fn load_dark() -> bool {
    settings_dir()
        .and_then(|d| std::fs::read_to_string(d.join("theme")).ok())
        .map(|t| t.trim() != "light")
        .unwrap_or(true)
}

pub fn save_dark(dark: bool) {
    if let Some(d) = settings_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("theme"), if dark { "dark" } else { "light" });
    }
}

pub fn load_flag(name: &str, default: bool) -> bool {
    settings_dir()
        .and_then(|d| std::fs::read_to_string(d.join(name)).ok())
        .map(|t| t.trim() == "1")
        .unwrap_or(default)
}

pub fn save_flag(name: &str, value: bool) {
    if let Some(d) = settings_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(name), if value { "1" } else { "0" });
    }
}

pub fn load_str(name: &str) -> Option<String> {
    settings_dir().and_then(|d| std::fs::read_to_string(d.join(name)).ok())
}

pub fn save_str(name: &str, value: &str) {
    if let Some(d) = settings_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(name), value);
    }
}
