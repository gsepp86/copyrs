//! Una sola finestra con una coda: le altre istanze lasciano il lavoro in una cartella e chiudono.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Job {
    pub sources: Vec<PathBuf>,
    pub dest: PathBuf,
    pub mv: bool,
}

fn qdir() -> Option<PathBuf> {
    let d = crate::shell::settings_dir()?.join("queue");
    fs::create_dir_all(&d).ok()?;
    Some(d)
}

fn age(p: &std::path::Path) -> Option<Duration> {
    fs::metadata(p).and_then(|m| m.modified()).ok()?.elapsed().ok()
}

fn stamp() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|t| t.as_nanos()).unwrap_or(0);
    format!("{nanos}-{}", std::process::id())
}

/// C'e' gia' una finestra CopyRs aperta (il suo "battito" e' recente)?
pub fn primary_alive() -> bool {
    qdir().and_then(|d| age(&d.join("instance.lock"))).map(|e| e < Duration::from_secs(2)).unwrap_or(false)
}

pub fn heartbeat() {
    if let Some(d) = qdir() {
        let _ = fs::write(d.join("instance.lock"), "1");
    }
}

pub fn send(job: &Job) -> bool {
    let Some(d) = qdir() else { return false };
    let name = stamp();
    let mut text = String::from(if job.mv { "move\n" } else { "copy\n" });
    text.push_str(&job.dest.to_string_lossy());
    text.push('\n');
    for s in &job.sources {
        text.push_str(&s.to_string_lossy());
        text.push('\n');
    }
    let tmp = d.join(format!("{name}.tmp"));
    fs::write(&tmp, text).is_ok() && fs::rename(&tmp, d.join(format!("{name}.job"))).is_ok()
}

pub fn take_all() -> Vec<Job> {
    let Some(d) = qdir() else { return Vec::new() };
    let Ok(rd) = fs::read_dir(&d) else { return Vec::new() };
    let mut files: Vec<PathBuf> =
        rd.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("job")).collect();
    files.sort();
    let mut jobs = Vec::new();
    for f in files {
        let fresh = age(&f).map(|e| e < Duration::from_secs(120)).unwrap_or(true);
        let text = fs::read_to_string(&f).unwrap_or_default();
        let _ = fs::remove_file(&f);
        if !fresh {
            continue;
        }
        let mut lines = text.lines();
        let mv = lines.next() == Some("move");
        let Some(dest) = lines.next().map(PathBuf::from) else { continue };
        let sources: Vec<PathBuf> = lines.filter(|l| !l.is_empty()).map(PathBuf::from).collect();
        if !sources.is_empty() {
            jobs.push(Job { sources, dest, mv });
        }
    }
    jobs
}

fn src_files(d: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match fs::read_dir(d) {
        Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("src")).collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

fn read_lines(f: &std::path::Path, out: &mut Vec<PathBuf>) {
    if let Ok(t) = fs::read_to_string(f) {
        out.extend(t.lines().filter(|l| !l.is_empty()).map(PathBuf::from));
    }
}

/// Chi raccoglie i percorsi di una selezione multipla. Tiene il "lucchetto" finche' non viene eliminato,
/// cosi' i processi che arrivano in ritardo (anche durante la scelta della cartella) lasciano i loro file e chiudono.
pub struct Gather {
    dir: Option<PathBuf>,
    stop: Arc<AtomicBool>,
}

pub enum Role {
    Leader(Gather),
    Follower,
}

/// Con una selezione multipla Explorer avvia un processo per file: uno solo diventa Leader e mostra
/// una sola scelta della destinazione, gli altri (Follower) lasciano il loro file e chiudono.
pub fn gather_start(own: &[PathBuf]) -> Role {
    let solo = || Role::Leader(Gather { dir: None, stop: Arc::new(AtomicBool::new(true)) });
    let Some(d) = qdir() else { return solo() };
    let text: String = own.iter().map(|p| format!("{}\n", p.to_string_lossy())).collect();
    let mine = d.join(format!("g-{}.src", stamp()));
    if fs::write(&mine, text).is_err() {
        return solo();
    }
    let lock = d.join("gather.lock");
    let try_lock = || fs::OpenOptions::new().write(true).create_new(true).open(&lock).is_ok();
    let mut got = try_lock();
    if !got && age(&lock).map_or(true, |e| e > Duration::from_secs(3)) {
        // Lucchetto rimasto da una sessione interrotta: i suoi vecchi percorsi vanno ignorati.
        let _ = fs::remove_file(&lock);
        for f in src_files(&d) {
            if f != mine {
                let _ = fs::remove_file(f);
            }
        }
        got = try_lock();
    }
    if !got {
        return Role::Follower;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (s2, l2) = (stop.clone(), lock.clone());
    std::thread::spawn(move || {
        while !s2.load(Relaxed) {
            let _ = fs::write(&l2, "1");
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    Role::Leader(Gather { dir: Some(d), stop })
}

impl Gather {
    /// Aspetta che smettano di arrivare nuovi file (al massimo 6 secondi).
    pub fn wait_quiet(&self, quiet_ms: u64) {
        let Some(d) = &self.dir else { return };
        let start = Instant::now();
        let mut last = src_files(d).len();
        let mut since = Instant::now();
        while since.elapsed() < Duration::from_millis(quiet_ms) && start.elapsed() < Duration::from_secs(6) {
            std::thread::sleep(Duration::from_millis(50));
            let n = src_files(d).len();
            if n != last {
                last = n;
                since = Instant::now();
            }
        }
    }

    /// Legge e cancella i percorsi raccolti finora.
    pub fn collect(&self) -> Vec<PathBuf> {
        let Some(d) = &self.dir else { return Vec::new() };
        let mut all = Vec::new();
        for f in src_files(d) {
            let fresh = age(&f).map(|e| e < Duration::from_secs(1800)).unwrap_or(true);
            if fresh {
                if let Ok(t) = fs::read_to_string(&f) {
                    all.extend(t.lines().filter(|l| !l.is_empty()).map(PathBuf::from));
                }
            }
            let _ = fs::remove_file(&f);
        }
        all
    }
}

impl Drop for Gather {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(d) = &self.dir {
            let _ = fs::remove_file(d.join("gather.lock"));
        }
    }
}
