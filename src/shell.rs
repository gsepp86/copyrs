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
            (format!(r"{ROOT}\*\shell\CopyRsMove"), "Sposta con CopyRs", format!("\"{exe}\" --move \"%1\"")),
            (format!(r"{ROOT}\Directory\shell\CopyRsMove"), "Sposta con CopyRs", format!("\"{exe}\" --move \"%1\"")),
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

    fn install_dir() -> Option<PathBuf> {
        Some(PathBuf::from(env::var_os("LOCALAPPDATA")?).join("Programs").join("CopyRs"))
    }

    fn installed_exe() -> Option<PathBuf> {
        install_dir().map(|d| d.join("copyrs.exe")).filter(|p| p.exists())
    }

    /// Percorso a cui puntano voci e collegamenti: la copia installata, se c'e', altrimenti l'exe in esecuzione.
    fn exe_str() -> io::Result<String> {
        let p = match installed_exe() {
            Some(p) => p,
            None => env::current_exe()?,
        };
        Ok(p.to_string_lossy().to_string())
    }

    pub fn installed_path() -> Option<String> {
        installed_exe().map(|p| p.to_string_lossy().to_string())
    }

    /// Copia l'exe in %LOCALAPPDATA%\Programs\CopyRs e attiva clic destro, "Invia a" e menu Start.
    pub fn install_app() -> io::Result<()> {
        let cur = env::current_exe()?;
        let dir = install_dir().ok_or_else(|| io::Error::new(io::ErrorKind::Other, "cartella del profilo non trovata"))?;
        fs::create_dir_all(&dir)?;
        let dst = dir.join("copyrs.exe");
        let a = fs::canonicalize(&cur).ok();
        let same = a.is_some() && a == fs::canonicalize(&dst).ok();
        if !same {
            fs::copy(&cur, &dst).map_err(|e| {
                io::Error::new(e.kind(), format!("copia in «{}»: {e} (chiudi le altre finestre di CopyRs e riprova)", dst.display()))
            })?;
        }
        install()?;
        create_shortcuts(false)
    }

    pub fn install() -> io::Result<()> {
        let exe = exe_str()?;
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
        let exe = exe_str().unwrap_or_default();
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
    pub fn create_shortcuts(desktop: bool) -> io::Result<()> {
        let exe = exe_str()?.replace('\'', "''");
        let dirs = if desktop { "@($desk,$start)" } else { "@($start)" };
        let script = format!(
            "$exe='{exe}'; $ws=New-Object -ComObject WScript.Shell; \
             $desk=[Environment]::GetFolderPath('Desktop'); \
             $start=Join-Path $env:APPDATA 'Microsoft\\Windows\\Start Menu\\Programs'; \
             $send=Join-Path $env:APPDATA 'Microsoft\\Windows\\SendTo'; \
             function Mk($dir,$name,$a){{ $s=$ws.CreateShortcut((Join-Path $dir ($name+'.lnk'))); $s.TargetPath=$exe; $s.Arguments=$a; $s.WorkingDirectory=(Split-Path $exe); $s.IconLocation=$exe; $s.Save() }}; \
             foreach($d in {dirs}){{ Mk $d 'CopyRs' ''; Mk $d 'CopyRs - Impostazioni' '--settings' }}; \
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

    const CLASSIC_KEY: &str = r"Software\Classes\CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}";

    pub fn classic_menu_enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!(r"{CLASSIC_KEY}\InprocServer32")).is_ok()
    }

    /// Attiva/disattiva il menu "classico" (voci subito visibili) per l'utente e riavvia Esplora file.
    pub fn set_classic_menu(on: bool) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if on {
            let (k, _) = hkcu.create_subkey(format!(r"{CLASSIC_KEY}\InprocServer32"))?;
            let empty = String::new();
            k.set_value("", &empty)?;
        } else {
            let _ = hkcu.delete_subkey_all(CLASSIC_KEY);
        }
        ps("Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2; \
            if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { Start-Process explorer.exe }; exit 0")
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
    pub fn create_shortcuts(_desktop: bool) -> io::Result<()> { Err(io::Error::new(io::ErrorKind::Unsupported, "solo Windows")) }
    pub fn install_app() -> io::Result<()> { Err(io::Error::new(io::ErrorKind::Unsupported, "solo Windows")) }
    pub fn installed_path() -> Option<String> { None }
    pub fn classic_menu_enabled() -> bool { false }
    pub fn set_classic_menu(_on: bool) -> io::Result<()> { Err(io::Error::new(io::ErrorKind::Unsupported, "solo Windows")) }
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
