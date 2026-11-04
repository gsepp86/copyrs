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
    pub current: AtomicUsize,
    pub file_done: AtomicU64,
    pub file_total: AtomicU64,
    pub phase: AtomicUsize, // 0 = copia, 1 = verifica
    pub paused: AtomicBool,
    pub cancel: AtomicBool,
    pub skip: AtomicBool,
    pub shutdown: AtomicBool,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            done_bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            current: AtomicUsize::new(0),
            file_done: AtomicU64::new(0),
            file_total: AtomicU64::new(0),
            phase: AtomicUsize::new(0),
            paused: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            skip: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        }
    }

    /// Aggiunge file alla coda (anche mentre la copia e' in corso).
    pub fn append(&self, items: Vec<Item>) {
        let w: u64 = items.iter().map(|i| i.weight()).sum();
        self.total_bytes.fetch_add(w, Relaxed);
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

fn copy_one(sh: &Shared, src: &Path, dst: &Path, verify: bool, copied: &mut u64) -> io::Result<Out> {
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

fn finish(sh: &Shared, i: usize, st: Status, add: u64) {
    sh.done_bytes.fetch_add(add, Relaxed);
    sh.items.lock().unwrap()[i].status = st;
}

/// Thread di copia: resta vivo e prende i file in coda uno alla volta, anche quelli aggiunti dopo.
pub fn run(sh: Arc<Shared>) {
    let mut cursor = 0usize;
    loop {
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
                finish(&sh, i, Status::Failed("origine e destinazione coincidono".into()), size * ph);
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
                finish(&sh, i, Status::Skipped, size * ph);
                continue;
            }
            if policy == Collision::Rename {
                dst = unique(&dst);
            }
        }
        let mut copied = 0u64;
        let st = match copy_one(&sh, &src, &dst, verify, &mut copied) {
            Ok(Out::Done) => {
                if mv {
                    let _ = fs::remove_file(&src);
                }
                Status::Done
            }
            Ok(Out::Mismatch) => Status::Mismatch,
            Ok(Out::Skipped) => Status::Skipped,
            Ok(Out::Cancelled) => Status::Cancelled,
            Err(e) => Status::Failed(e.to_string()),
        };
        finish(&sh, i, st, (size * ph).saturating_sub(copied));
    }
}
