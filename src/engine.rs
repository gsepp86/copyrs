use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::{io, thread, time::{Duration, SystemTime}};

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Collision {
    #[default]
    Overwrite,
    Skip,
    Newer,
    Rename,
}

impl Collision {
    pub fn label(self) -> &'static str {
        match self {
            Self::Overwrite => "Sovrascrivi",
            Self::Skip => "Salta",
            Self::Newer => "Solo se più recenti",
            Self::Rename => "Rinomina",
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum Status {
    Waiting,
    Copying,
    Done,
    Skipped,
    Cancelled,
    Mismatch,
    Failed(String),
}

pub struct Item {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub size: u64,
    pub status: Status,
    pub mv: bool,
    pub verify: bool,
    pub policy: Collision,
}

impl Item {
    /// Peso nell'avanzamento: la verifica conta come una seconda lettura del file.
    pub fn weight(&self) -> u64 {
        self.size * if self.verify { 2 } else { 1 }
    }
}

pub struct Shared {
    pub items: Mutex<Vec<Item>>,
    pub done_bytes: AtomicU64,
    pub total_bytes: AtomicU64,
    pub done_size: AtomicU64,
    pub total_size: AtomicU64,
    pub current: AtomicUsize,
    pub file_done: AtomicU64,
    pub file_total: AtomicU64,
    pub phase: AtomicUsize, // 0 = copia, 1 = verifica
    pub paused: AtomicBool,
    pub cancel: AtomicBool,
    pub skip: AtomicBool,
    pub shutdown: AtomicBool,
    pub failed_items: Mutex<Vec<(usize, String)>>,
    pub retry: AtomicBool,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            done_bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            done_size: AtomicU64::new(0),
            total_size: AtomicU64::new(0),
            current: AtomicUsize::new(0),
            file_done: AtomicU64::new(0),
            file_total: AtomicU64::new(0),
            phase: AtomicUsize::new(0),
            paused: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            skip: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            failed_items: Mutex::new(Vec::new()),
            retry: AtomicBool::new(false),
        }
    }

    /// Aggiunge file alla coda (anche mentre la copia e' in corso).
    pub fn append(&self, items: Vec<Item>) {
        let w: u64 = items.iter().map(|i| i.weight()).sum();
        self.total_bytes.fetch_add(w, Relaxed);
        self.total_size.fetch_add(items.iter().map(|i| i.size).sum::<u64>(), Relaxed);
        self.items.lock().unwrap().extend(items);
    }

    pub fn is_active(&self) -> bool {
        self.items.lock().unwrap().iter().any(|i| matches!(i.status, Status::Waiting | Status::Copying))
    }
}

/// Espande file e cartelle in una lista piatta di file da copiare.
pub fn collect(sources: &[PathBuf], dest: &Path, mv: bool, verify: bool) -> Vec<Item> {
    let mut out = Vec::new();
    for s in sources {
        let base = s.parent().unwrap_or(Path::new("")).to_path_buf();
        walk(s, &base, dest, mv, verify, &mut out);
    }
    out
}

fn walk(p: &Path, base: &Path, dest: &Path, mv: bool, verify: bool, out: &mut Vec<Item>) {
    let Ok(md) = fs::metadata(p) else { return };
    if md.is_dir() {
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                walk(&e.path(), base, dest, mv, verify, out);
            }
        }
    } else {
        let rel = p.strip_prefix(base).unwrap_or(p);
        out.push(Item {
            src: p.to_path_buf(),
            dst: dest.join(rel),
            size: md.len(),
            status: Status::Waiting,
            mv,
            verify,
            policy: Collision::Overwrite,
        });
    }
}

pub fn apply_policy(items: &mut [Item], p: Collision) {
    for it in items.iter_mut() {
        it.policy = p;
    }
}

fn diff_secs(a: SystemTime, b: SystemTime) -> i64 {
    match a.duration_since(b) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// "Uguale" = stessa dimensione e stessa data di modifica (tolleranza 2 s, per FAT/exFAT).
pub fn is_same(src: &Path, dst: &Path) -> bool {
    let (Ok(a), Ok(b)) = (fs::metadata(src), fs::metadata(dst)) else { return false };
    match (a.modified(), b.modified()) {
        (Ok(x), Ok(y)) => a.len() == b.len() && diff_secs(x, y).abs() < 2,
        _ => false,
    }
}

fn is_newer(src: &Path, dst: &Path) -> bool {
    let m = |p: &Path| fs::metadata(p).and_then(|m| m.modified());
    match (m(src), m(dst)) {
        (Ok(x), Ok(y)) => diff_secs(x, y) >= 2,
        _ => true,
    }
}

/// Restituisce (file già presenti in destinazione, di cui identici).
pub fn conflicts(items: &[Item]) -> (usize, usize) {
    let (mut existing, mut same) = (0, 0);
    for it in items {
        if it.dst.exists() {
            existing += 1;
            if is_same(&it.src, &it.dst) {
                same += 1;
            }
        }
    }
    (existing, same)
}

fn unique(p: &Path) -> PathBuf {
    let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 1.. {
        let c = p.with_file_name(format!("{stem} ({i}){ext}"));
        if !c.exists() {
            return c;
        }
    }
    unreachable!()
}

enum Out {
    Done,
    Mismatch,
    Skipped,
    Cancelled,
}

/// Aggiunge al messaggio d'errore quale operazione e quale file hanno fallito.
fn ctx<T>(r: io::Result<T>, what: &str, p: &Path) -> io::Result<T> {
    r.map_err(|e| io::Error::new(e.kind(), format!("{what} «{}»: {e}", p.display())))
}

fn stop_requested(sh: &Shared) -> Option<Out> {
    while sh.paused.load(Relaxed) && !sh.cancel.load(Relaxed) && !sh.skip.load(Relaxed) {
        thread::sleep(Duration::from_millis(100));
    }
    if sh.cancel.load(Relaxed) {
        Some(Out::Cancelled)
    } else if sh.skip.load(Relaxed) {
        Some(Out::Skipped)
    } else {
        None
    }
}

fn copy_one(sh: &Shared, src: &Path, dst: &Path, verify: bool, copied: &mut u64, copied_actual: &mut u64) -> io::Result<Out> {
    if let Some(p) = dst.parent() {
        ctx(fs::create_dir_all(p), "creazione della cartella", p)?;
    }
    let mut r = ctx(File::open(src), "apertura dell'origine", src)?;
    let mut w = ctx(File::create(dst), "creazione della destinazione", dst)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut h = crc32fast::Hasher::new();
    let mut since_sync = 0u64;
    loop {
        if let Some(out) = stop_requested(sh) {
            drop(w);
            let _ = fs::remove_file(dst);
            return Ok(out);
        }
        let n = ctx(r.read(&mut buf), "lettura dell'origine", src)?;
        if n == 0 {
            break;
        }
        ctx(w.write_all(&buf[..n]), "scrittura della destinazione", dst)?;
        h.update(&buf[..n]);
        *copied += n as u64;
        *copied_actual += n as u64;
        sh.done_bytes.fetch_add(n as u64, Relaxed);
        sh.file_done.fetch_add(n as u64, Relaxed);
        // Scrittura reale su disco ogni 16 MB: cosi' la barra segue la velocita' vera e non quella della cache.
        since_sync += n as u64;
        if since_sync >= (16u64 << 20) {
            ctx(w.sync_data(), "scrittura su disco", dst)?;
            since_sync = 0;
        }
    }
    ctx(w.flush(), "scrittura della destinazione", dst)?;
    ctx(w.sync_all(), "scrittura su disco", dst)?;
    drop(w);
    if verify {
        let expected = h.finalize();
        let mut f = ctx(File::open(dst), "apertura della copia per la verifica", dst)?;
        sh.phase.store(1, Relaxed);
        let mut h2 = crc32fast::Hasher::new();
        loop {
            if let Some(out) = stop_requested(sh) {
                drop(f);
                let _ = fs::remove_file(dst);
                return Ok(out);
            }
            let n = ctx(f.read(&mut buf), "lettura della copia", dst)?;
            if n == 0 {
                break;
            }
            h2.update(&buf[..n]);
            *copied += n as u64;
            sh.done_bytes.fetch_add(n as u64, Relaxed);
            sh.file_done.fetch_add(n as u64, Relaxed);
        }
        if h2.finalize() != expected {
            return Ok(Out::Mismatch);
        }
    }
    if let Ok(m) = fs::metadata(src).and_then(|m| m.modified()) {
        let _ = File::options().write(true).open(dst).and_then(|f| f.set_modified(m));
    }
    Ok(Out::Done)
}

fn finish(sh: &Shared, i: usize, st: Status, add_w: u64, add_a: u64) {
    sh.done_bytes.fetch_add(add_w, Relaxed);
    sh.done_size.fetch_add(add_a, Relaxed);
    sh.items.lock().unwrap()[i].status = st;
}

/// Thread di copia: resta vivo e prende i file in coda uno alla volta, anche quelli aggiunti dopo.
pub fn run(sh: Arc<Shared>) {
    let mut cursor = 0usize;
    loop {
        // Dopo un "Retry" i file falliti davanti al cursore vanno ripresi.
        if sh.retry.swap(false, Relaxed) {
            cursor = 0;
        }
        if sh.shutdown.load(Relaxed) {
            break;
        }
        let claimed = {
            let mut v = sh.items.lock().unwrap();
            let mut found = None;
            while cursor < v.len() {
                if v[cursor].status == Status::Waiting {
                    v[cursor].status = Status::Copying;
                    let it = &v[cursor];
                    found = Some((cursor, it.src.clone(), it.dst.clone(), it.size, it.mv, it.verify, it.policy));
                    break;
                }
                cursor += 1;
            }
            found
        };
        let Some((i, src, mut dst, size, mv, verify, policy)) = claimed else {
            thread::sleep(Duration::from_millis(100));
            continue;
        };
        cursor = i + 1;

        // Controllo dello spazio PRIMA di copiare: se sul volume di destinazione
        // non c'e' posto per la coda, i file non vengono nemmeno aperti.
        if let Some(free) = free_space_at(&dst) {
            let mut need = 0u64;
            let mut targets: Vec<usize> = Vec::new();
            {
                let v = sh.items.lock().unwrap();
                for (k, it) in v.iter().enumerate() {
                    if matches!(it.status, Status::Waiting | Status::Copying) && same_volume(&it.dst, &dst) {
                        need = need.saturating_add(it.size);
                        targets.push(k);
                    }
                }
            }
            if need > free {
                let msg = format!(
                    "spazio insufficiente sul disco di destinazione: servono {} ma ne restano {}",
                    fmt_bytes(need),
                    fmt_bytes(free)
                );
                {
                    let mut v = sh.items.lock().unwrap();
                    let mut failed = sh.failed_items.lock().unwrap();
                    for k in targets {
                        if matches!(v[k].status, Status::Waiting | Status::Copying) {
                            let w = v[k].weight();
                            let sz = v[k].size;
                            v[k].status = Status::Failed(msg.clone());
                            sh.done_bytes.fetch_add(w, Relaxed);
                            sh.done_size.fetch_add(sz, Relaxed);
                            failed.push((k, msg.clone()));
                        }
                    }
                }
                continue;
            }
        }

        let ph: u64 = if verify { 2 } else { 1 };
        sh.current.store(i, Relaxed);
        sh.skip.store(false, Relaxed);
        sh.cancel.store(false, Relaxed);
        sh.file_done.store(0, Relaxed);
        sh.file_total.store(size * ph, Relaxed);
        sh.phase.store(0, Relaxed);

        // Sicurezza: mai copiare un file su se stesso (lo troncherebbe).
        if let (Ok(a), Ok(b)) = (src.canonicalize(), dst.canonicalize()) {
            if a == b {
                finish(&sh, i, Status::Failed("origine e destinazione coincidono".into()), size * ph, size);
                continue;
            }
        }
        if dst.exists() {
            let skip = match policy {
                Collision::Skip => true,
                Collision::Newer => !is_newer(&src, &dst),
                _ => false,
            };
            if skip {
                finish(&sh, i, Status::Skipped, size * ph, size);
                continue;
            }
            if policy == Collision::Rename {
                dst = unique(&dst);
            }
        }
        let mut copied = 0u64;
        let mut copied_actual = 0u64;
        let st = match copy_one(&sh, &src, &dst, verify, &mut copied, &mut copied_actual) {
            Ok(Out::Done) => {
                if mv {
                    let _ = fs::remove_file(&src);
                }
                Status::Done
            }
            Ok(Out::Mismatch) => Status::Mismatch,
            Ok(Out::Skipped) => Status::Skipped,
            Ok(Out::Cancelled) => Status::Cancelled,
            Err(e) => {
                // Non lasciare sul disco una copia parziale: occupa spazio e inganna.
                let _ = fs::remove_file(&dst);
                let why = if is_disk_full(&e) {
                    format!("spazio esaurito sul disco di destinazione durante la scrittura: {e}")
                } else {
                    format!("copia non riuscita: {e}")
                };
                Status::Failed(why)
            }
        };
        match &st {
            Status::Failed(m) => sh.failed_items.lock().unwrap().push((i, m.clone())),
            Status::Mismatch => sh.failed_items
                .lock()
                .unwrap()
                .push((i, "il CRC32 della copia non corrisponde all'originale".into())),
            _ => {}
        }
        finish(&sh, i, st, (size * ph).saturating_sub(copied), size.saturating_sub(copied_actual));
    }
}

/// Spazio libero disponibile per l'utente sul volume che contiene `p`.
/// Su Windows usa GetDiskFreeSpaceExW (API di sistema, nessuna dipendenza in piu').
#[cfg(windows)]
fn free_space_at(p: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }
    let dir = nearest_dir(p)?;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) };
    if ok != 0 { Some(free) } else { None }
}

#[cfg(not(windows))]
fn free_space_at(_p: &Path) -> Option<u64> {
    None
}

/// Prima cartella esistente risalendo dal percorso (la destinazione puo' non esistere ancora).
#[cfg(windows)]
fn nearest_dir(p: &Path) -> Option<PathBuf> {
    let mut cur = if p.is_dir() { Some(p.to_path_buf()) } else { p.parent().map(Path::to_path_buf) };
    while let Some(c) = cur {
        if c.is_dir() {
            return Some(c);
        }
        cur = c.parent().map(Path::to_path_buf);
    }
    None
}

/// Vero se i due percorsi sono sullo stesso volume (stessa lettera di unita' o stesso root).
fn same_volume(a: &Path, b: &Path) -> bool {
    use std::path::Component;
    match (a.components().next(), b.components().next()) {
        (Some(Component::Prefix(x)), Some(Component::Prefix(y))) => x == y,
        (Some(Component::RootDir), Some(Component::RootDir)) => true,
        _ => false,
    }
}

fn is_disk_full(e: &io::Error) -> bool {
    // 112 = ERROR_DISK_FULL (Windows), 28 = ENOSPC (Unix).
    matches!(e.raw_os_error(), Some(112) | Some(28))
}

/// Byte in forma leggibile, es. "1.4 GB".
fn fmt_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    let n = n as f64;
    if n >= KB * KB * KB {
        format!("{:.1} GB", n / (KB * KB * KB))
    } else if n >= KB * KB {
        format!("{:.0} MB", n / (KB * KB))
    } else if n >= KB {
        format!("{:.0} KB", n / KB)
    } else {
        format!("{:.0} B", n)
    }
}
