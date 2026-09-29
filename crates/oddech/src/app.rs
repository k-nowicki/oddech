//! Application shell: file opening (dialog, CLI, drag & drop), background
//! import, and dispatch of long-running actions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, bounded};
use egui::{RichText, Ui};
use oddech_media::{ImportProgress, ImportedMedia, MediaError};

use crate::jobs::{self, Job, JobOutcome};
use crate::session::Session;
use crate::settings::{ExportFormat, Settings};
use crate::ui::options::LabelFormat;
use crate::ui::{self, Action, UiState, theme, widgets};

enum State {
    Empty,
    Loading {
        path: PathBuf,
        progress: Arc<ImportProgress>,
        rx: Receiver<Result<ImportedMedia, MediaError>>,
    },
    Ready(Box<Session>),
}

pub struct OddechApp {
    settings: Settings,
    state: State,
    ui: UiState,
    cache_root: PathBuf,
    /// Pending result of a native file dialog running off the UI thread.
    dialog: Option<Receiver<(Dialog, Option<PathBuf>)>>,
    /// Our window, so dialogs open on top of it and give focus back.
    dialog_parent: Option<DialogParent>,
    /// Settings as last written to disk; changes are saved right away so
    /// they survive a crash or Ctrl+C in the terminal.
    saved_settings: Settings,
    last_error: Option<String>,
    job: Option<Job>,
    frame_stats: Option<FrameStats>,
}

/// Frame-time statistics, enabled with `ODDECH_DEBUG_FPS=1` (used to verify
/// the 60 fps target on long recordings).
#[derive(Default)]
struct FrameStats {
    samples: Vec<f32>,
    since: Option<std::time::Instant>,
}

impl FrameStats {
    fn record(&mut self, ms: f32) {
        self.samples.push(ms);
        let since = *self.since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed().as_secs_f32() >= 2.0 && self.samples.len() > 10 {
            let mut s = std::mem::take(&mut self.samples);
            s.sort_by(f32::total_cmp);
            let p = |q: f32| s[((s.len() - 1) as f32 * q) as usize];
            log::info!("ui frame cpu time: n={} median={:.2} ms p95={:.2} ms max={:.2} ms", s.len(), p(0.5), p(0.95), p(1.0));
            self.since = None;
        }
    }
}

impl OddechApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        theme::install(&cc.egui_ctx);
        let settings = Settings::load();
        let mut app = Self {
            saved_settings: settings.clone(),
            settings,
            state: State::Empty,
            ui: UiState::default(),
            cache_root: oddech_core::cache::default_cache_root(),
            dialog: None,
            dialog_parent: DialogParent::of(cc),
            last_error: None,
            job: None,
            frame_stats: std::env::var_os("ODDECH_DEBUG_FPS").map(|_| FrameStats::default()),
        };
        if let Some(path) = initial {
            app.open(&cc.egui_ctx, path);
        }
        // Load the detection model now, so the status bar can say where it
        // runs and the first detection starts at once.
        let ctx = cc.egui_ctx.clone();
        std::thread::Builder::new()
            .name("oddech-model".into())
            .stack_size(64 << 20)
            .spawn(move || {
                if let Err(e) = oddech_detect::respiro::Respiro::shared() {
                    log::error!("{e}");
                }
                ctx.request_repaint();
            })
            .expect("spawn model thread");
        app
    }

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        if let State::Ready(session) = &mut self.state {
            session.stop();
            session.autosave(true);
        }
        if let Some(dir) = path.parent() {
            self.settings.last_dir = Some(dir.to_owned());
        }
        self.last_error = None;
        let progress = Arc::new(ImportProgress::default());
        let (tx, rx) = bounded(1);
        let (p, root, prog, ctx2) = (path.clone(), self.cache_root.clone(), Arc::clone(&progress), ctx.clone());
        std::thread::Builder::new()
            .name("oddech-import".into())
            .spawn(move || {
                let result = oddech_media::import_media(&p, &root, &prog);
                let _ = tx.send(result);
                ctx2.request_repaint();
            })
            .expect("spawn import thread");
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("Oddech — {}", file_name(&path))));
        self.state = State::Loading { path, progress, rx };
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let dir = self.settings.last_dir.clone();
        self.spawn_dialog(ctx, Dialog::Open, move |d| {
            let mut d = d
                .set_title("Otwórz nagranie")
                .add_filter("Wideo i audio", &["mkv", "mp4", "mov", "m4v", "webm", "avi", "mts", "wav", "flac", "mp3", "m4a"])
                .add_filter("Wszystkie pliki", &["*"]);
            if let Some(dir) = dir {
                d = d.set_directory(dir);
            }
            d.pick_file()
        });
    }

    /// Asks where to write an export: a file, or a folder for per-track
    /// WAVs. Starts in the recording's folder with a name that never
    /// replaces the original.
    fn export_dialog(&mut self, ctx: &egui::Context, what: Dialog) {
        let State::Ready(session) = &self.state else { return };
        let media = session.media_path.clone();
        let dir = media.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = media.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (title, name, filter): (&str, String, Option<(&str, String)>) = match what {
            Dialog::Export if self.settings.export_format == ExportFormat::WavTracks => {
                ("Wybierz katalog na pliki WAV", String::new(), None)
            }
            Dialog::Export => {
                let ext = media.extension().and_then(|e| e.to_str()).unwrap_or("mkv").to_owned();
                ("Eksportuj wideo", format!("{stem}-oddech.{ext}"), Some(("Wideo", ext)))
            }
            Dialog::Timeline => {
                ("Zapisz oś czasu", format!("{stem}-oddech.fcpxml"), Some(("FCPXML", "fcpxml".to_owned())))
            }
            Dialog::Labels(f) => {
                let ext = match f {
                    LabelFormat::Csv => "csv",
                    LabelFormat::Json => "json",
                };
                ("Zapisz etykiety", format!("{stem}-etykiety.{ext}"), Some(("Etykiety", ext.to_owned())))
            }
            Dialog::Open => return,
        };
        self.spawn_dialog(ctx, what, move |d| {
            let d = d.set_title(title).set_directory(&dir);
            match filter {
                None => d.pick_folder(),
                Some((label, ext)) => d.set_file_name(&name).add_filter(label, &[ext.as_str()]).save_file(),
            }
        });
    }

    fn spawn_dialog(
        &mut self,
        ctx: &egui::Context,
        what: Dialog,
        show: impl FnOnce(rfd::FileDialog) -> Option<PathBuf> + Send + 'static,
    ) {
        if self.dialog.is_some() {
            return;
        }
        let (tx, rx) = bounded(1);
        let parent = self.dialog_parent;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut d = rfd::FileDialog::new();
            if let Some(parent) = &parent {
                d = d.set_parent(parent);
            }
            let _ = tx.send((what, show(d)));
            ctx.request_repaint();
        });
        self.dialog = Some(rx);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.dialog
            && let Ok(result) = rx.try_recv()
        {
            self.dialog = None;
            // Back to the front, whatever the window manager did meanwhile.
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            match result {
                (Dialog::Open, Some(path)) => self.open(ctx, path),
                (what, Some(path)) => self.export_to(ctx, what, path),
                (_, None) => {}
            }
        }
        if let State::Loading { rx, path, .. } = &self.state
            && let Ok(result) = rx.try_recv()
        {
            match result {
                Ok(media) => {
                    log::info!(
                        "opened {} ({} tracks, {:.1} s, from cache: {})",
                        path.display(),
                        media.tracks.len(),
                        media.info.duration_secs,
                        media.from_cache
                    );
                    let keep = media.cache.dir().to_owned();
                    self.state = State::Ready(Box::new(Session::new(media)));
                    self.housekeep_cache(ctx, Some(keep));
                }
                Err(e) => {
                    self.last_error = Some(format!("Nie udało się otworzyć {}:\n{e}", file_name(path)));
                    self.state = State::Empty;
                }
            }
        }
        // Drag & drop.
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_owned()));
        if let Some(path) = dropped {
            self.open(ctx, path);
        }
    }

    /// Writes an export to the place chosen in the dialog.
    fn export_to(&mut self, ctx: &egui::Context, what: Dialog, target: PathBuf) {
        let State::Ready(session) = &mut self.state else { return };
        let same = |a: &Path, b: &Path| a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b);
        if same(&target, &session.media_path) {
            self.ui.error("Nie można zapisać eksportu w miejsce oryginalnego nagrania.");
            return;
        }
        match what {
            Dialog::Export => {
                if self.job.is_some() {
                    return;
                }
                session.autosave(true);
                match jobs::start_export(session, &self.settings, target, ctx) {
                    Ok(job) => self.job = Some(job),
                    Err(e) => self.ui.error(e),
                }
            }
            Dialog::Timeline => match jobs::export_timeline(session, self.settings.timeline_start_hours, &target) {
                Ok(()) => self.ui.toast(format!("Zapisano oś czasu: {}", file_name(&target))),
                Err(e) => self.ui.error(format!("Nie udało się zapisać osi czasu: {e}")),
            },
            Dialog::Labels(format) => match jobs::export_labels(session, format, &target) {
                Ok(()) => self.ui.toast(format!("Zapisano etykiety: {}", file_name(&target))),
                Err(e) => self.ui.error(format!("Nie udało się zapisać etykiet: {e}")),
            },
            Dialog::Open => {}
        }
    }

    /// Frees the cache down to its limit, off the UI thread; `keep` is the
    /// open recording.
    fn housekeep_cache(&mut self, ctx: &egui::Context, keep: Option<PathBuf>) {
        let (root, limit) = (self.cache_root.clone(), u64::from(self.settings.cache_limit_gb) << 30);
        self.spawn_cache_task(ctx, move || {
            oddech_core::cache::evict(&root, limit, keep.as_slice());
        });
    }

    /// Runs `task` in the background, then measures the cache for the UI.
    fn spawn_cache_task(&mut self, ctx: &egui::Context, task: impl FnOnce() + Send + 'static) {
        let root = self.cache_root.clone();
        let bytes = std::sync::Arc::clone(&self.ui.cache_bytes);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            task();
            let total = oddech_core::cache::entries(&root).iter().map(|e| e.bytes).sum();
            bytes.store(total, std::sync::atomic::Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    fn open_cache_dir(&self) -> Option<PathBuf> {
        match &self.state {
            State::Ready(s) => Some(s.cache.dir().to_owned()),
            _ => None,
        }
    }

    fn run_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::MeasureCache => return self.spawn_cache_task(ctx, || {}),
            Action::ClearCache => {
                let (root, keep) = (self.cache_root.clone(), self.open_cache_dir());
                return self.spawn_cache_task(ctx, move || {
                    oddech_core::cache::clear(&root, keep.as_slice());
                });
            }
            _ => {}
        }
        if matches!(action, Action::Open) {
            self.open_dialog(ctx);
            return;
        }
        let State::Ready(session) = &mut self.state else { return };
        match action {
            Action::Open | Action::MeasureCache | Action::ClearCache => {}
            Action::Export => {
                if self.job.is_some() {
                    return;
                }
                self.export_dialog(ctx, Dialog::Export);
            }
            Action::ExportTimeline => self.export_dialog(ctx, Dialog::Timeline),
            Action::ExportLabels(format) => self.export_dialog(ctx, Dialog::Labels(format)),
            Action::Detect => {
                if self.job.is_some() {
                    self.ui.toast("Poczekaj na zakończenie bieżącego zadania.");
                    return;
                }
                let ids: Vec<_> =
                    session.tracks.iter().map(|t| t.info.id).filter(|id| session.project.track(*id).fix).collect();
                session.pending_detection.clear();
                match jobs::start_detection(session, &ids, ctx) {
                    Ok(job) => self.job = Some(job),
                    Err(e) => self.ui.error(e),
                }
            }
        }
    }

    fn poll_job(&mut self) {
        let Some(job) = &self.job else {
            self.ui.busy = false;
            self.ui.job_status = None;
            return;
        };
        if self.ui.cancel_requested {
            self.ui.cancel_requested = false;
            job.cancel();
        }
        self.ui.busy = true;
        self.ui.job_status = Some(job.describe());
        if let Some(outcome) = job.poll() {
            match outcome {
                JobOutcome::Detected(results) => {
                    if let State::Ready(session) = &mut self.state {
                        session.apply_detections(results);
                        let found = session.review_list().len();
                        self.ui.toast(format!("Wykryto {}.", crate::ui::topbar::breath_count_label(found)));
                    }
                }
                JobOutcome::Exported(paths) => {
                    let names: Vec<String> = paths.iter().map(|p| file_name(p)).collect();
                    self.ui.toast(format!("Gotowe: {}", names.join(", ")));
                }
                JobOutcome::Failed(e) => self.ui.error(format!("{} nie powiódł się: {e}", job.title)),
                JobOutcome::Cancelled => self.ui.toast(format!("{} przerwany.", job.title)),
            }
            self.job = None;
            self.ui.busy = false;
            self.ui.job_status = None;
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl eframe::App for OddechApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let frame_start = std::time::Instant::now();
        self.draw(ui);
        // Not while a slider is being dragged: once, when it is let go.
        if self.settings != self.saved_settings && !ui.input(|i| i.pointer.any_down()) {
            if self.settings.cache_limit_gb < self.saved_settings.cache_limit_gb {
                let keep = self.open_cache_dir();
                self.housekeep_cache(ui.ctx(), keep);
            }
            self.settings.save();
            self.saved_settings = self.settings.clone();
        }
        if let Some(stats) = &mut self.frame_stats {
            stats.record(frame_start.elapsed().as_secs_f32() * 1000.0);
            ui.ctx().request_repaint();
        }
    }

    fn on_exit(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
        self.settings.save();
        if let State::Ready(session) = &mut self.state {
            session.stop();
            session.autosave(true);
        }
    }
}

impl OddechApp {
    fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        if ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O))) {
            self.open_dialog(&ctx);
        }

        self.poll_job();
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F1)) {
            ui::help::toggle(&mut self.ui);
        }
        ui::statusbar::show(ui, &mut self.ui);
        // With a recording open the help panel sits below the top bar
        // (`ui::show_session`); otherwise it is placed here.
        if self.ui.help_open && !matches!(self.state, State::Ready(_)) {
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.ui.help_open = false;
            }
            ui::help::show(ui, &mut self.ui);
        }
        // Automatic detection for tracks just switched to "Popraw wdechy".
        if self.job.is_none()
            && let State::Ready(session) = &mut self.state
            && !session.pending_detection.is_empty()
        {
            let ids = std::mem::take(&mut session.pending_detection);
            match jobs::start_detection(session, &ids, &ctx) {
                Ok(job) => self.job = Some(job),
                Err(e) => self.ui.error(e),
            }
        }
        match &mut self.state {
            State::Ready(session) => {
                let actions = ui::show_session(ui, session, &mut self.settings, &mut self.ui);
                for a in actions {
                    self.run_action(&ctx, a);
                }
            }
            State::Loading { path, progress, .. } => {
                let (name, fraction) = (file_name(path), progress.fraction() as f32);
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
                    centered(ui, |ui| {
                        ui.label(RichText::new(name).font(theme::bold(20.0)));
                        ui.add_space(12.0);
                        ui.add(egui::ProgressBar::new(fraction).desired_width(360.0).desired_height(8.0));
                        ui.add_space(8.0);
                        ui.label(RichText::new("Wczytywanie ścieżek audio…").color(theme::TEXT_SECONDARY));
                    });
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            State::Empty => {
                let mut open = false;
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
                    centered(ui, |ui| {
                        ui.label(RichText::new("Oddech").font(theme::bold(34.0)));
                        ui.add_space(4.0);
                        ui.label(RichText::new("Wykrywanie i ściszanie wdechów w nagraniach").size(16.0).color(theme::TEXT_SECONDARY));
                        ui.add_space(28.0);
                        if widgets::primary_button(ui, "  Otwórz nagranie…  ").clicked() {
                            open = true;
                        }
                        ui.add_space(12.0);
                        ui.label(RichText::new("lub upuść plik wideo w tym oknie · Ctrl+O").color(theme::TEXT_TERTIARY));
                        if let Some(e) = &self.last_error {
                            ui.add_space(24.0);
                            ui.label(RichText::new(e).color(theme::DANGER));
                        }
                    });
                });
                if open {
                    self.open_dialog(&ctx);
                }
            }
        }
    }

}

fn centered(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let rect = ui.available_rect_before_wrap();
    let inner = egui::Rect::from_center_size(rect.center(), egui::vec2(520.0, 260.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Center)),
    );
    add(&mut child);
}

/// What a file dialog was opened for.
#[derive(Debug, Clone, Copy)]
enum Dialog {
    Open,
    Export,
    Timeline,
    Labels(LabelFormat),
}

/// Our main window as a dialog parent (X11 or Wayland), sent to the dialog
/// thread. The file portal ties the dialog to the window: on X11 by the
/// window id, on Wayland by a handle exported through xdg-foreign.
#[derive(Clone, Copy)]
struct DialogParent {
    window: raw_window_handle::RawWindowHandle,
    display: raw_window_handle::RawDisplayHandle,
}

// SAFETY: the handles name our main window and its display connection,
// which live until the process ends. The dialog thread only passes them to
// rfd, which uses the Wayland display from its own thread by design
// (libwayland connections are thread-safe).
unsafe impl Send for DialogParent {}

impl DialogParent {
    fn of(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
        Some(Self { window: cc.window_handle().ok()?.as_raw(), display: cc.display_handle().ok()?.as_raw() })
    }
}

impl raw_window_handle::HasWindowHandle for DialogParent {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        // SAFETY: see `Send` above.
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(self.window) })
    }
}

impl raw_window_handle::HasDisplayHandle for DialogParent {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        // SAFETY: see `Send` above.
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(self.display) })
    }
}
