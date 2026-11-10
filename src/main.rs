#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]
mod engine;
mod ipc;
mod shell;

use eframe::egui::{self, Color32};
use egui_extras::{Column, TableBuilder};
use engine::{Collision, Item, Shared, Status};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RED: Color32 = Color32::from_rgb(230, 60, 60);
const ORANGE: Color32 = Color32::from_rgb(230, 150, 0);
const PAYPAL_URL: &str = "https://paypal.me/PeppeCP";
const GITHUB_URL: &str = "https://github.com/gsepp86/copyrs";

#[derive(Default)]
struct CopyApp {
    sources: Vec<PathBuf>,
    dest: Option<PathBuf>,
    mv: bool,
    verify: bool,
    sh: Option<Arc<Shared>>,
    pending: Vec<(Vec<Item>, usize, usize)>,
    first_run: bool,
    msg: String,
    only_errors: bool,
    settings: bool,
    dark: bool,
    autostart: bool,
    autoclose: bool,
    done_at: Option<Instant>,
    speed: f64,
    samp: Option<(Instant, u64)>,
    jobs: Option<Receiver<ipc::Job>>,
    size: usize,
    about: bool,
}

fn fmt(b: u64) -> String {
    let u = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (b as f64, 0);
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, u[i])
}

/// Testi e pulsanti (riapplicato a ogni frame: vale anche dopo il cambio tema).
fn apply_style(ctx: &egui::Context) {
    use egui::{FontId, Stroke, TextStyle};
    ctx.style_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(22.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        let accent = Color32::from_rgb(80, 140, 240);
        let (f0, f1, f2) = if s.visuals.dark_mode {
            (Color32::from_rgb(52, 60, 78), Color32::from_rgb(68, 80, 104), Color32::from_rgb(40, 48, 66))
        } else {
            (Color32::from_rgb(214, 224, 244), Color32::from_rgb(196, 210, 238), Color32::from_rgb(180, 196, 230))
        };
        let w = &mut s.visuals.widgets;
        w.inactive.weak_bg_fill = f0;
        w.inactive.bg_stroke = Stroke::new(1.5, accent);
        w.hovered.weak_bg_fill = f1;
        w.hovered.bg_stroke = Stroke::new(1.5, accent);
        w.active.weak_bg_fill = f2;
        w.active.bg_stroke = Stroke::new(1.5, accent);
    });
}

/// Dimensioni predefinite della finestra: 0 = piccola, 1 = media, 2 = estesa.
fn dims(i: usize) -> (f32, f32) {
    match i {
        1 => (800.0, 440.0),
        2 => (1100.0, 620.0),
        _ => (720.0, 400.0),
    }
}

fn load_size() -> usize {
    shell::load_str("size").and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(0).min(2)
}

fn is_err(s: &Status) -> bool {
    matches!(s, Status::Failed(_) | Status::Mismatch)
}

fn err_msg(s: &Status) -> String {
    match s {
        Status::Failed(e) => e.clone(),
        Status::Mismatch => "il CRC32 della copia non corrisponde all'originale".into(),
        _ => String::new(),
    }
}

impl CopyApp {
    fn new(sources: Vec<PathBuf>, dest: Option<PathBuf>, mv: bool, auto: bool, jobs: Option<Receiver<ipc::Job>>) -> Self {
        Self {
            verify: shell::load_flag("verify", true),
            sources,
            dest,
            mv,
            first_run: shell::needs_prompt(),
            dark: shell::load_dark(),
            autostart: auto,
            autoclose: shell::load_flag("autoclose", true),
            jobs,
            size: load_size(),
            ..Default::default()
        }
    }

    fn begin(&mut self) {
        let Some(dest) = self.dest.clone() else { return };
        let job = ipc::Job { sources: self.sources.clone(), dest, mv: self.mv };
        self.enqueue(job);
    }

    /// Prepara un lavoro: se ci sono file gia' presenti chiede cosa fare, altrimenti lo mette subito in coda.
    fn enqueue(&mut self, job: ipc::Job) {
        let items = engine::collect(&job.sources, &job.dest, job.mv, self.verify);
        if items.is_empty() {
            return;
        }
        let (existing, same) = engine::conflicts(&items);
        if existing == 0 {
            self.launch(items);
        } else {
            self.pending.push((items, existing, same));
        }
    }

    /// Se la coda e' ferma parte un elenco nuovo (gli errori precedenti spariscono), altrimenti si accoda.
    fn launch(&mut self, items: Vec<Item>) {
        let idle = self.sh.as_ref().map_or(true, |sh| !sh.is_active());
        if idle {
            if let Some(old) = self.sh.take() {
                old.shutdown.store(true, Relaxed);
            }
            let sh = Arc::new(Shared::new());
            let s2 = sh.clone();
            std::thread::spawn(move || engine::run(s2));
            self.sh = Some(sh);
            self.speed = 0.0;
            self.samp = None;
            self.only_errors = false;
        }
        self.done_at = None;
        if let Some(sh) = &self.sh {
            sh.append(items);
        }
    }

    fn first_run_ui(&mut self, ui: &mut egui::Ui) {
        ui.set_max_width(640.0);
        ui.heading("Integrazione con Windows");
        ui.label("CopyRs può aggiungere a Esplora file le voci «Copia con CopyRs» e «Incolla con CopyRs» nel menu contestuale (clic destro su file, cartelle e sfondo di una cartella).");
        ui.add_space(6.0);
        ui.weak("Attenzione: Windows non permette a un programma normale di sostituire la finestra di copia di sistema. Ctrl+C / Ctrl+V e il trascinamento in Esplora file continuano a usare quella originale. Le modifiche valgono solo per il tuo utente e si annullano dalle Impostazioni.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Attiva integrazione").clicked() {
                match shell::install() {
                    Ok(()) => {
                        shell::mark_done();
                        self.first_run = false;
                        self.msg.clear();
                    }
                    Err(e) => self.msg = format!("Non riuscito: {e}"),
                }
            }
            if ui.button("Non ora").clicked() {
                shell::mark_done();
                self.first_run = false;
                self.msg.clear();
            }
        });
        if !self.msg.is_empty() {
            ui.label(&self.msg);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Impostazioni");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Indietro").clicked() {
                    self.settings = false;
                    self.msg.clear();
                }
            });
        });
        ui.separator();
        if !self.msg.is_empty() {
            ui.colored_label(Color32::from_rgb(80, 140, 240), self.msg.clone());
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(640.0);
            ui.strong("Tema");
            let before = self.dark;
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.dark, true, "Scuro");
                ui.radio_value(&mut self.dark, false, "Chiaro");
            });
            if self.dark != before {
                ui.ctx().set_theme(if self.dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
                shell::save_dark(self.dark);
            }

            ui.add_space(10.0);
            ui.strong("Dimensione della finestra");
            let before_size = self.size;
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.size, 0, "Piccola");
                ui.radio_value(&mut self.size, 1, "Media");
                ui.radio_value(&mut self.size, 2, "Estesa");
            });
            if self.size != before_size {
                shell::save_str("size", &self.size.to_string());
                let (w, h) = dims(self.size);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
            }

            ui.add_space(10.0);
            ui.strong("Copia");
            if ui.checkbox(&mut self.verify, "Verifica CRC32 dopo la copia").changed() {
                shell::save_flag("verify", self.verify);
            }
            if ui.checkbox(&mut self.autoclose, "Chiudi la finestra se tutti i file sono stati copiati correttamente").changed() {
                shell::save_flag("autoclose", self.autoclose);
            }

            if cfg!(windows) {
                ui.add_space(10.0);
                ui.strong("Integrazione con Windows");
                ui.label("Aggiunge «Copia con CopyRs» e «Incolla con CopyRs» al menu contestuale di Esplora file. Non sostituisce Ctrl+C / Ctrl+V. Su Windows 11 la voce si trova in clic destro → «Mostra altre opzioni».");
                let (ok, total) = shell::status();
                if total > 0 && ok == total {
                    ui.colored_label(Color32::from_rgb(40, 170, 80), format!("Stato: attiva ({ok}/{total} voci)"));
                } else if ok == 0 {
                    ui.weak("Stato: non attiva");
                } else {
                    ui.colored_label(ORANGE, format!("Stato: incompleta o con percorso diverso ({ok}/{total}). Premi «Attiva integrazione» per ripararla."));
                }
                ui.horizontal(|ui| {
                    if ui.button("Attiva integrazione").clicked() {
                        self.msg = match shell::install() {
                            Ok(()) => "Integrazione attivata. Su Windows 11: clic destro → Mostra altre opzioni.".into(),
                            Err(e) => format!("Non riuscito: {e}"),
                        };
                    }
                    if ui.button("Rimuovi integrazione").clicked() {
                        self.msg = match shell::uninstall() {
                            Ok(()) => "Integrazione rimossa.".into(),
                            Err(e) => format!("Non riuscito: {e}"),
                        };
                    }
                });

                ui.add_space(10.0);
                ui.strong("Collegamenti");
                ui.label("Crea «CopyRs» e «CopyRs - Impostazioni» sul Desktop e nel menu Start, più la voce «Invia a → CopyRs» (utile con molti file). Per la barra delle applicazioni: dal menu Start cerca CopyRs, clic destro → «Aggiungi alla barra delle applicazioni» (Windows non permette di farlo in automatico).");
                ui.horizontal(|ui| {
                    if ui.button("Crea collegamenti").clicked() {
                        self.msg = match shell::create_shortcuts() {
                            Ok(()) => "Collegamenti creati: Desktop, menu Start e Invia a.".into(),
                            Err(e) => format!("Non riuscito: {e}"),
                        };
                    }
                    if ui.button("Rimuovi collegamenti").clicked() {
                        self.msg = match shell::remove_shortcuts() {
                            Ok(()) => "Collegamenti rimossi.".into(),
                            Err(e) => format!("Non riuscito: {e}"),
                        };
                    }
                });
            }
        });
    }

    fn about_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Informazioni");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Indietro").clicked() {
                    self.about = false;
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(640.0);
            ui.heading("CopyRs – copia file in modo semplice e controllato");
            ui.add_space(10.0);
            ui.label("Se vuoi contribuire al mio lavoro ti lascio in basso il pulsante per contribuire anche con una piccola donazione simbolica visto che una app così utile gratis per sempre non la trovi ovunque apprezzo il gesto ;-)");
            ui.add_space(6.0);
            let pay = egui::Button::new(egui::RichText::new("Dona con PayPal").strong().color(Color32::WHITE))
                .fill(Color32::from_rgb(0, 112, 186))
                .min_size(egui::vec2(170.0, 34.0));
            if ui.add(pay).on_hover_text(PAYPAL_URL).clicked() {
                shell::open_url(PAYPAL_URL);
            }
            ui.add_space(12.0);
            ui.label("Un copiatore di file leggero, gratuito e open source scritto in Rust, ispirato ai classici copiatori di file per Windows: una finestra compatta, una coda di copia e il controllo che i file siano arrivati davvero integri.");
            ui.add_space(12.0);
            if ui.link(GITHUB_URL).clicked() {
                shell::open_url(GITHUB_URL);
            }
        });
    }

    /// Finestra al centro con la domanda sui file gia' presenti (vale per il primo lavoro in attesa).
    fn conflict_window(&mut self, ctx: &egui::Context) {
        let Some((_, existing, same)) = self.pending.first() else { return };
        let (existing, same) = (*existing, *same);
        let mut choice: Option<Option<Collision>> = None;
        egui::Window::new("File già presenti")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.label(format!(
                    "{existing} file con lo stesso nome esistono già nella destinazione, di cui {same} identici (stessa dimensione e data)."
                ));
                ui.label("Cosa vuoi fare?");
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for p in [Collision::Overwrite, Collision::Skip, Collision::Newer, Collision::Rename] {
                        if ui.button(p.label()).clicked() {
                            choice = Some(Some(p));
                        }
                    }
                });
                ui.add_space(6.0);
                if ui.button("Annulla").clicked() {
                    choice = Some(None);
                }
            });
        if let Some(c) = choice {
            let (mut items, _, _) = self.pending.remove(0);
            if let Some(p) = c {
                engine::apply_policy(&mut items, p);
                self.launch(items);
            }
        }
    }

    fn setup_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("CopyRs");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Impostazioni").clicked() {
                    self.settings = true;
                }
                let info = egui::Button::new(egui::RichText::new("i").strong()).min_size(egui::vec2(30.0, 0.0));
                if ui.add(info).on_hover_text("Informazioni").clicked() {
                    self.about = true;
                }
            });
        });
        ui.add_space(6.0);
        ui.columns(2, |cols| {
            let (l, r) = cols.split_at_mut(1);
            let (l, r) = (&mut l[0], &mut r[0]);

            l.strong("File e cartelle da copiare");
            l.horizontal(|ui| {
                if ui.button("Aggiungi file…").clicked() {
                    if let Some(f) = rfd::FileDialog::new().pick_files() {
                        self.sources.extend(f);
                    }
                }
                if ui.button("Aggiungi cartella…").clicked() {
                    if let Some(f) = rfd::FileDialog::new().pick_folder() {
                        self.sources.push(f);
                    }
                }
                if ui.button("Svuota").clicked() {
                    self.sources.clear();
                }
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(l, |ui| {
                if self.sources.is_empty() {
                    ui.weak("Trascina qui file e cartelle oppure usa i pulsanti.");
                }
                for s in &self.sources {
                    ui.label(s.display().to_string());
                }
            });

            r.strong("Destinazione");
            r.horizontal_wrapped(|ui| {
                ui.label(self.dest.as_ref().map(|d| d.display().to_string()).unwrap_or("—".into()));
                if ui.button("Sfoglia…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.dest = Some(d);
                    }
                }
            });
            r.add_space(10.0);
            r.checkbox(&mut self.mv, "Sposta invece di copiare");
            if r.checkbox(&mut self.verify, "Verifica CRC32 dopo la copia").changed() {
                shell::save_flag("verify", self.verify);
            }
            r.add_space(14.0);
            let ready = !self.sources.is_empty() && self.dest.is_some();
            let go = egui::Button::new(egui::RichText::new("Avvia").strong().color(Color32::WHITE))
                .fill(Color32::from_rgb(40, 110, 220))
                .min_size(egui::vec2(110.0, 32.0));
            if r.add_enabled(ready, go).clicked() {
                self.begin();
            }
        });
    }

    fn progress_ui(&mut self, ctx: &egui::Context, sh: &Arc<Shared>) {
        let done = sh.done_bytes.load(Relaxed);
        let total = sh.total_bytes.load(Relaxed);
        let mut items = sh.items.lock().unwrap();
        let finished = !items.iter().any(|i| matches!(i.status, Status::Waiting | Status::Copying));
        let cur = sh.current.load(Relaxed).min(items.len().saturating_sub(1));
        let (mut ok, mut bad, mut skipped, mut waiting) = (0usize, 0usize, 0usize, 0usize);
        for it in items.iter() {
            match it.status {
                Status::Done => ok += 1,
                Status::Failed(_) | Status::Mismatch => bad += 1,
                Status::Skipped | Status::Cancelled => skipped += 1,
                Status::Waiting => waiting += 1,
                Status::Copying => {}
            }
        }
        let verifying = sh.phase.load(Relaxed) == 1;

        // Velocita' sugli ultimi secondi (non sul tempo totale).
        if finished {
            self.speed = 0.0;
            self.samp = None;
        } else {
            match self.samp {
                None => self.samp = Some((Instant::now(), done)),
                Some((t, d0)) => {
                    let dt = t.elapsed().as_secs_f64();
                    if dt >= 0.5 {
                        let inst = done.saturating_sub(d0) as f64 / dt;
                        self.speed = if self.speed == 0.0 { inst } else { self.speed * 0.6 + inst * 0.4 };
                        self.samp = Some((Instant::now(), done));
                    }
                }
            }
        }
        let eta = if !finished && self.speed > 1.0 { Some((total.saturating_sub(done) as f64 / self.speed) as u64) } else { None };

        if finished && bad == 0 && skipped == 0 && self.autoclose && self.pending.is_empty() && !items.is_empty() {
            let t = *self.done_at.get_or_insert_with(Instant::now);
            if t.elapsed() > Duration::from_millis(1200) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else {
            self.done_at = None;
        }

        let (mut act_cancel, mut act_skip_cur, mut act_reset) = (false, false, false);
        let mut skip_rows: Vec<usize> = Vec::new();

        egui::SidePanel::right("side").exact_width(320.0).resizable(false).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if finished {
                    if ui.button("Torna all'inizio").clicked() {
                        act_reset = true;
                    }
                } else {
                    let paused = sh.paused.load(Relaxed);
                    if ui.button(if paused { "Riprendi" } else { "Pausa" }).clicked() {
                        sh.paused.store(!paused, Relaxed);
                    }
                    if ui.button("Salta file").clicked() {
                        act_skip_cur = true;
                    }
                    let stop = egui::Button::new(egui::RichText::new("Annulla tutto").color(Color32::WHITE))
                        .fill(Color32::from_rgb(170, 45, 45));
                    if ui.add(stop).clicked() {
                        act_cancel = true;
                    }
                }
            });
            ui.separator();
            if let Some(it) = items.get(cur) {
                ui.label(format!("Da: {}", it.src.display()));
                ui.label(format!("A:  {}", it.dst.display()));
            }
            ui.add_space(4.0);
            let ft = sh.file_total.load(Relaxed);
            let fd = sh.file_done.load(Relaxed);
            let ffrac = if ft == 0 { 0.0 } else { (fd as f32 / ft as f32).min(1.0) };
            ui.label(if finished {
                "Terminato"
            } else if sh.paused.load(Relaxed) {
                "In pausa"
            } else if verifying {
                "Verifica del file…"
            } else {
                "Copia del file…"
            });
            ui.add(egui::ProgressBar::new(ffrac).show_percentage().fill(Color32::from_rgb(60, 130, 230)));
            ui.add_space(2.0);
            ui.label("Totale");
            let mut bar = egui::ProgressBar::new((done as f32 / total.max(1) as f32).min(1.0))
                .show_percentage()
                .fill(Color32::from_rgb(46, 160, 90));
            if bad > 0 {
                bar = bar.fill(RED);
            }
            ui.add(bar);
            ui.label(format!("{} / {}", fmt(done), fmt(total)));
            if !finished {
                let eta_txt = eta.map(|e| format!(" — mancano {}:{:02}", e / 60, e % 60)).unwrap_or_default();
                ui.label(format!("{}/s{}", fmt(self.speed as u64), eta_txt));
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(Color32::GREEN, format!("Copiati: {ok}"));
                ui.colored_label(Color32::YELLOW, format!("Saltati: {skipped}"));
                ui.colored_label(if bad > 0 { RED } else { Color32::GRAY }, format!("Errori: {bad}"));
                ui.label(format!("In coda: {waiting}"));
            });
            ui.separator();
            if finished {
                if bad == 0 && skipped == 0 {
                    let m = if self.autoclose { "Completato senza errori. Chiusura automatica…" } else { "Completato senza errori." };
                    ui.colored_label(Color32::GREEN, m);
                } else if bad == 0 {
                    ui.colored_label(ORANGE, format!("Completato: {skipped} file saltati o annullati."));
                } else {
                    ui.colored_label(RED, format!("Completato: {bad} file con errori. La finestra resta aperta."));
                    ui.checkbox(&mut self.only_errors, "Mostra solo errori");
                    if ui.button("Copia elenco errori").clicked() {
                        let text = items
                            .iter()
                            .filter(|i| is_err(&i.status))
                            .map(|i| format!("{}\t{}", i.src.display(), err_msg(&i.status)))
                            .collect::<Vec<_>>()
                            .join("\n");
                        ui.output_mut(|o| o.copied_text = text);
                    }
                }
            } else {
                ui.weak("Le altre copie avviate con «Copia con CopyRs» vengono aggiunte alla coda in questa finestra.");
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let idx: Vec<usize> =
                (0..items.len()).filter(|&i| !self.only_errors || is_err(&items[i].status)).collect();
            let rh = ui.text_style_height(&egui::TextStyle::Body) + 3.0;
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .column(Column::exact(100.0))
                .column(Column::initial(200.0).clip(true))
                .column(Column::exact(86.0))
                .column(Column::remainder().clip(true))
                .header(24.0, |mut h| {
                    h.col(|ui| {
                        ui.strong("Stato");
                    });
                    h.col(|ui| {
                        ui.strong("File");
                    });
                    h.col(|ui| {
                        ui.strong("Dimensione");
                    });
                    h.col(|ui| {
                        ui.strong("Destinazione / dettaglio");
                    });
                })
                .body(|body| {
                    body.rows(rh, idx.len(), |mut row| {
                        let i = idx[row.index()];
                        let it = &items[i];
                        let name = it.src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        let (txt, col): (&str, Color32) = match &it.status {
                            Status::Waiting => ("In coda", Color32::GRAY),
                            Status::Copying => (if verifying { "Verifica…" } else { "Copia…" }, Color32::LIGHT_BLUE),
                            Status::Done => ("OK", Color32::GREEN),
                            Status::Skipped => ("Saltato", Color32::YELLOW),
                            Status::Cancelled => ("Annullato", Color32::YELLOW),
                            Status::Mismatch => ("CRC diverso", RED),
                            Status::Failed(_) => ("Errore", RED),
                        };
                        let is_bad = is_err(&it.status);
                        row.col(|ui| {
                            ui.colored_label(col, txt);
                        });
                        row.col(|ui| {
                            if is_bad {
                                ui.colored_label(RED, name);
                            } else {
                                ui.label(name);
                            }
                        });
                        row.col(|ui| {
                            ui.label(fmt(it.size));
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                if matches!(it.status, Status::Waiting | Status::Copying) && ui.small_button("Salta").clicked() {
                                    skip_rows.push(i);
                                }
                                if is_bad {
                                    let m = err_msg(&it.status);
                                    ui.colored_label(RED, m.clone()).on_hover_text(m);
                                } else {
                                    ui.weak(it.dst.display().to_string());
                                }
                            });
                        });
                    });
                });
        });

        // Azioni richieste dall'utente (applicate dopo il disegno, a elenco libero).
        if act_cancel {
            for it in items.iter_mut() {
                if it.status == Status::Waiting {
                    it.status = Status::Cancelled;
                    sh.done_bytes.fetch_add(it.weight(), Relaxed);
                }
            }
            sh.paused.store(false, Relaxed);
            sh.cancel.store(true, Relaxed);
        }
        if act_skip_cur {
            sh.skip.store(true, Relaxed);
        }
        for i in skip_rows {
            let st = items[i].status.clone();
            match st {
                Status::Waiting => {
                    let w = items[i].weight();
                    items[i].status = Status::Skipped;
                    sh.done_bytes.fetch_add(w, Relaxed);
                }
                Status::Copying => sh.skip.store(true, Relaxed),
                _ => {}
            }
        }
        if act_reset {
            sh.shutdown.store(true, Relaxed);
            self.sh = None;
            self.only_errors = false;
            self.done_at = None;
        }
    }
}

impl eframe::App for CopyApp {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        apply_style(ctx);

        // Lavori arrivati da altre "Copia con CopyRs": finiscono in questa stessa coda.
        let incoming: Vec<ipc::Job> = match &self.jobs {
            Some(rx) => rx.try_iter().collect(),
            None => Vec::new(),
        };
        if !incoming.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        for j in incoming {
            self.enqueue(j);
        }

        if self.autostart && !self.first_run && !self.sources.is_empty() && self.sh.is_none() && self.pending.is_empty() {
            self.autostart = false;
            self.begin();
        }
        if self.sh.is_none() && self.pending.is_empty() {
            for f in ctx.input(|i| i.raw.dropped_files.clone()) {
                if let Some(p) = f.path {
                    self.sources.push(p);
                }
            }
        }

        let sh = self.sh.clone();
        match &sh {
            Some(sh) => self.progress_ui(ctx, sh),
            None => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if self.first_run {
                        self.first_run_ui(ui);
                    } else if self.about {
                        self.about_ui(ui);
                    } else if self.settings {
                        self.settings_ui(ui);
                    } else {
                        self.setup_ui(ui);
                    }
                });
            }
        }
        if !self.pending.is_empty() {
            self.conflict_window(ctx);
        }
        if sh.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn main() -> eframe::Result {
    // File/cartelle passati da riga di comando (menu contestuale, Invia a), --paste <cartella> o --settings.
    let mut raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let open_settings = raw.first().map_or(false, |a| a.as_os_str() == "--settings");
    if open_settings {
        raw.remove(0);
    }
    let paste = raw.first().map_or(false, |a| a.as_os_str() == "--paste");
    let (mut sources, mut dest, mv) = if paste {
        let (files, cut) = shell::clipboard_files();
        (files, raw.get(1).map(PathBuf::from), cut)
    } else {
        (raw.into_iter().map(PathBuf::from).collect::<Vec<_>>(), None, false)
    };

    // Selezione multipla: Explorer avvia un processo per file, solo uno prosegue con tutti i percorsi.
    let mut guard: Option<ipc::Gather> = None;
    if !paste && !sources.is_empty() {
        match ipc::gather_start(&sources) {
            ipc::Role::Follower => return Ok(()),
            ipc::Role::Leader(g) => {
                g.wait_quiet(500);
                sources.extend(g.collect());
                guard = Some(g);
            }
        }
    }

    // La prima istanza e' quella con la finestra: le altre le passano il lavoro e chiudono.
    let primary = !ipc::primary_alive();
    if primary {
        ipc::heartbeat();
        std::thread::spawn(|| loop {
            ipc::heartbeat();
            std::thread::sleep(Duration::from_millis(500));
        });
    }

    let mut auto = paste;
    if !paste && !sources.is_empty() {
        // Una sola domanda sulla destinazione per tutti i file, poi parte la copia.
        let picked = rfd::FileDialog::new().set_title("CopyRs — scegli la cartella di destinazione").pick_folder();
        if let Some(g) = guard.take() {
            g.wait_quiet(300); // file dei processi arrivati mentre sceglievi la cartella
            sources.extend(g.collect());
        }
        sources.sort();
        sources.dedup();
        match picked {
            Some(d) => {
                dest = Some(d);
                auto = true;
            }
            None => return Ok(()),
        }
    }
    if !primary && auto && !sources.is_empty() {
        if let Some(d) = &dest {
            if ipc::send(&ipc::Job { sources: sources.clone(), dest: d.clone(), mv }) {
                return Ok(());
            }
        }
    }

    let (w, h) = dims(load_size());
    let icon = egui::IconData { rgba: include_bytes!("../assets/icon_128.rgba").to_vec(), width: 128, height: 128 };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([w, h])
            .with_min_inner_size([640.0, 340.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "CopyRs",
        opts,
        Box::new(move |cc| {
            let (tx, rx) = std::sync::mpsc::channel::<ipc::Job>();
            if primary {
                let ctx = cc.egui_ctx.clone();
                std::thread::spawn(move || loop {
                    for j in ipc::take_all() {
                        if tx.send(j).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                    std::thread::sleep(Duration::from_millis(300));
                });
            }
            let mut app = CopyApp::new(sources, dest, mv, auto, Some(rx));
            app.settings = open_settings;
            cc.egui_ctx.set_theme(if app.dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
            Ok(Box::new(app))
        }),
    )
}
