//! "Więcej opcji" side panel: rarely needed settings, grouped as in the
//! mockup, with sub-pages for track info, markers and training data.

use egui::{RichText, Ui};
use oddech_core::gain::{DEFAULT_FADE_MS, MUTE_FLOOR_DB};
use oddech_core::time::format_hms_millis;

use super::{Action, UiState, theme, widgets};
use crate::session::Session;
use crate::settings::{Colormap, ExportFormat, Settings, round_preroll};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Main,
    Discrimination,
    TrackInfo,
    Timeline,
    TrainingData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelFormat {
    Csv,
    Json,
}

pub fn show(ui: &mut Ui, session: &mut Session, settings: &mut Settings, state: &mut UiState, actions: &mut Vec<Action>) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(18.0);
        match state.options_page {
            Page::Main => main_page(ui, session, settings, state, actions),
            Page::Discrimination => discrimination_page(ui, session, state),
            Page::TrackInfo => track_info_page(ui, session, state),
            Page::Timeline => timeline_page(ui, session, settings, state, actions),
            Page::TrainingData => training_page(ui, session, settings, state, actions),
        }
        ui.add_space(24.0);
    });
}

fn header(ui: &mut Ui, state: &mut UiState, title: &str, subtitle: Option<&str>) {
    ui.horizontal(|ui| {
        if state.options_page != Page::Main {
            if widgets::icon_button(ui, "‹", "Wstecz").clicked() {
                state.options_page = Page::Main;
            }
        }
        ui.label(RichText::new(title).font(theme::bold(26.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button(ui, "✕", "Zamknij (Esc)").clicked() {
                state.options_open = false;
            }
        });
    });
    if let Some(s) = subtitle {
        ui.label(RichText::new(s).size(16.0).color(theme::TEXT_SECONDARY));
    }
}

fn main_page(ui: &mut Ui, session: &mut Session, settings: &mut Settings, state: &mut UiState, actions: &mut Vec<Action>) {
    header(ui, state, "Więcej opcji", Some("Domyślne wartości działają. Tu zaglądasz rzadko."));

    // --- Wykrywanie -------------------------------------------------------
    widgets::section_title(ui, "Wykrywanie");
    widgets::card(ui, |ui| {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Czułość").size(16.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{:.2}", sensitivity(settings.threshold))).font(theme::mono(15.0)).color(theme::TEXT_SECONDARY));
            });
        });
        // Sensitivity is the inverse of the confidence threshold: higher
        // sensitivity accepts less certain detections.
        let mut sens = sensitivity(settings.threshold);
        ui.spacing_mut().slider_width = ui.available_width();
        let r = ui.add(egui::Slider::new(&mut sens, 0.0..=1.0).show_value(false));
        if r.changed() {
            settings.threshold = 1.0 - sens;
        }
        ui.add_space(6.0);
        widgets::row_separator(ui);
        if widgets::nav_row(ui, "Dyskryminacja").clicked() {
            state.options_page = Page::Discrimination;
        }
        widgets::row_separator(ui);
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            let enabled = !state.busy && session.tracks.iter().any(|t| session.project.track(t.info.id).fix);
            let link = ui.add_enabled(enabled, egui::Button::new(RichText::new("Wykryj ponownie").size(16.0).color(theme::ACCENT)).frame(false));
            if link.on_disabled_hover_text("Włącz „Popraw wdechy” na co najmniej jednej ścieżce").clicked() {
                actions.push(Action::Detect);
            }
        });
    });

    // --- Przegląd ---------------------------------------------------------
    widgets::section_title(ui, "Przegląd");
    widgets::card(ui, |ui| {
        widgets::row(ui, "Start przed wdechem", |ui| {
            ui.label(RichText::new("s").size(16.0).color(theme::TEXT_SECONDARY));
            let mut v = settings.preroll_secs;
            let r = ui.add(
                egui::DragValue::new(&mut v)
                    .range(0.0..=60.0)
                    .speed(0.1)
                    .fixed_decimals(1)
                    .update_while_editing(false),
            );
            if r.changed() {
                settings.preroll_secs = round_preroll(v);
            }
        });
        widgets::row_separator(ui);
        widgets::row(ui, "Odtwarzaj tylko wdech", |ui| {
            widgets::toggle(ui, &mut settings.play_only_breath);
        });
        widgets::row_separator(ui);
        widgets::row(ui, "Przewijaj za głowicą", |ui| {
            widgets::toggle(ui, &mut settings.follow_playhead);
        });
        widgets::row_separator(ui);
        widgets::row(ui, "Zapętlaj zaznaczenie", |ui| {
            widgets::toggle(ui, &mut settings.loop_selection);
        });
    });

    // --- Zaznaczony wdech -------------------------------------------------
    widgets::section_title(ui, "Zaznaczony wdech");
    selected_region_card(ui, session);

    // --- Widok -------------------------------------------------------------
    widgets::section_title(ui, "Widok");
    widgets::card(ui, |ui| {
        widgets::row(ui, "Spektrogram zamiast przebiegu", |ui| {
            widgets::toggle(ui, &mut settings.show_spectrogram);
        });
        if settings.show_spectrogram {
            let sp = &mut settings.spectrogram;
            widgets::row_separator(ui);
            widgets::row(ui, "   Skala logarytmiczna", |ui| {
                widgets::toggle(ui, &mut sp.log_frequency);
            });
            widgets::row_separator(ui);
            widgets::row(ui, "   Zakres", |ui| {
                ui.label(RichText::new("dB").color(theme::TEXT_SECONDARY));
                ui.add(egui::DragValue::new(&mut sp.db_range).range(30.0..=140.0).speed(1.0).fixed_decimals(0));
            });
            widgets::row_separator(ui);
            widgets::row(ui, "   Mapa kolorów", |ui| {
                egui::ComboBox::from_id_salt("colormap").selected_text(sp.colormap.label()).show_ui(ui, |ui| {
                    for c in Colormap::ALL {
                        ui.selectable_value(&mut sp.colormap, c, c.label());
                    }
                });
            });
            widgets::row_separator(ui);
            widgets::row(ui, "   Okno FFT", |ui| {
                egui::ComboBox::from_id_salt("fft").selected_text(sp.fft_size.to_string()).show_ui(ui, |ui| {
                    for n in [256usize, 512, 1024, 2048, 4096] {
                        ui.selectable_value(&mut sp.fft_size, n, n.to_string());
                    }
                });
            });
        }
        widgets::row_separator(ui);
        widgets::row(ui, "Rozdziel kanały stereo", |ui| {
            widgets::toggle(ui, &mut settings.split_channels);
        });
        widgets::row_separator(ui);
        if widgets::nav_row(ui, "Informacje o ścieżkach").clicked() {
            state.options_page = Page::TrackInfo;
        }
    });

    // --- Eksport -----------------------------------------------------------
    widgets::section_title(ui, "Eksport");
    widgets::card(ui, |ui| {
        widgets::row(ui, "Format", |ui| {
            egui::ComboBox::from_id_salt("export_format")
                .selected_text(settings.export_format.label())
                .show_ui(ui, |ui| {
                    for f in ExportFormat::ALL {
                        ui.selectable_value(&mut settings.export_format, f, f.label());
                    }
                });
        });
        if settings.export_format == ExportFormat::WavTracks {
            widgets::row_separator(ui);
            widgets::row(ui, "   Próbki", |ui| {
                use crate::settings::WavDepth;
                let label = |d: WavDepth| if d == WavDepth::Float32 { "32-bit float" } else { "24-bit" };
                egui::ComboBox::from_id_salt("wav_depth").selected_text(label(settings.wav_depth)).show_ui(ui, |ui| {
                    for d in [WavDepth::Float32, WavDepth::Int24] {
                        ui.selectable_value(&mut settings.wav_depth, d, label(d));
                    }
                });
            });
        }
        widgets::row_separator(ui);
        if widgets::nav_row(ui, "Oś czasu do montażu (FCPXML)").clicked() {
            state.options_page = Page::Timeline;
        }
        widgets::row_separator(ui);
        if widgets::nav_row(ui, "Dane treningowe").clicked() {
            state.options_page = Page::TrainingData;
        }
    });

    // --- Pomoc -------------------------------------------------------------
    widgets::section_title(ui, "Pomoc");
    widgets::card(ui, |ui| {
        if widgets::nav_row(ui, "Skróty i opis działania (F1)").clicked() {
            super::help::toggle(state);
        }
    });

    // --- Dane nagrania -----------------------------------------------------
    widgets::section_title(ui, "Dane nagrania");
    widgets::card(ui, |ui| {
        let reference = oddech_core::project::Project::reference_path_for(&session.media_path);
        let rows = [
            ("Nagranie", Some(session.media_path.clone())),
            ("Projekt (poprawki)", Some(session.project_path().to_owned())),
            ("Kopia referencyjna", reference.exists().then_some(reference)),
            ("Pamięć podręczna", Some(session.cache.dir().to_owned())),
        ];
        for (i, (label, path)) in rows.into_iter().enumerate() {
            if i > 0 {
                widgets::row_separator(ui);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(16.0));
                if let Some(p) = &path {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Pokaż w folderze").clicked() {
                            show_in_folder(p);
                        }
                    });
                }
            });
            let text = path.as_ref().map_or("jeszcze nie ma (powstaje po „Zatwierdź wszystkie”)".to_owned(), |p| p.display().to_string());
            ui.add(egui::Label::new(RichText::new(text).size(13.0).color(theme::TEXT_SECONDARY)).wrap());
            ui.add_space(6.0);
        }
    });

    // --- Pamięć podręczna ---------------------------------------------------
    widgets::section_title(ui, "Pamięć podręczna");
    widgets::card(ui, |ui| {
        if state.cache_stale {
            state.cache_stale = false;
            actions.push(Action::MeasureCache);
        }
        let bytes = state.cache_bytes.load(std::sync::atomic::Ordering::Relaxed);
        let gb = |b: u64| b as f64 / f64::from(1u32 << 30);
        widgets::row(ui, "Zajęte", |ui| {
            let text = if bytes == u64::MAX {
                "liczę…".to_owned()
            } else {
                format!("{:.1} z {} GB", gb(bytes), settings.cache_limit_gb)
            };
            ui.label(RichText::new(text).font(theme::mono(14.0)).color(theme::TEXT_SECONDARY));
        });
        widgets::row_separator(ui);
        widgets::row(ui, "Limit", |ui| {
            ui.label(RichText::new("GB").color(theme::TEXT_SECONDARY));
            ui.add(
                egui::DragValue::new(&mut settings.cache_limit_gb)
                    .range(1..=10_000)
                    .speed(1.0)
                    .update_while_editing(false),
            );
        });
        ui.label(
            RichText::new("Zdekodowany dźwięk nagrań (ok. 4 GB na godzinę przy 3 ścieżkach stereo). Po przekroczeniu limitu usuwane są najdawniej otwierane; zawsze można je odtworzyć.")
                .size(13.0)
                .color(theme::TEXT_SECONDARY),
        );
        widgets::row_separator(ui);
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            let clear = egui::Button::new(RichText::new("Wyczyść (oprócz otwartego nagrania)").size(16.0).color(theme::ACCENT)).frame(false);
            if ui.add(clear).clicked() {
                state.cache_bytes.store(u64::MAX, std::sync::atomic::Ordering::Relaxed);
                actions.push(Action::ClearCache);
            }
        });
    });
}

fn sensitivity(threshold: f32) -> f32 {
    (1.0 - threshold).clamp(0.0, 1.0)
}

fn selected_region_card(ui: &mut Ui, session: &mut Session) {
    let Some(region) = session.selected_region.and_then(|id| session.project.region(id)).cloned() else {
        widgets::card(ui, |ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("Zaznacz wdech na osi czasu, aby ustawić dla niego osobne ściszenie.").color(theme::TEXT_SECONDARY));
            ui.add_space(10.0);
        });
        return;
    };
    let track = session.project.track(region.track);
    let id = region.id;
    widgets::card(ui, |ui| {
        widgets::row(ui, "Własne ściszenie", |ui| {
            let mut own = region.gain_db.is_some();
            if widgets::toggle(ui, &mut own).changed() {
                let v = own.then_some(session.gain_db());
                session.edit(|p| {
                    if let Some(r) = p.region_mut(id) {
                        r.gain_db = v;
                    }
                });
            }
        });
        if let Some(db) = region.gain_db {
            let mut v = if db.is_finite() { db.max(MUTE_FLOOR_DB) } else { MUTE_FLOOR_DB };
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = ui.available_width() - 80.0;
                let r = ui.add(egui::Slider::new(&mut v, MUTE_FLOOR_DB..=0.0).show_value(false).step_by(1.0));
                ui.label(RichText::new(if v <= MUTE_FLOOR_DB { "wycisz".into() } else { format!("{v:.0} dB") }).font(theme::mono(14.0)));
                if r.drag_started() {
                    session.begin_gesture();
                }
                if r.changed() {
                    let v = if v <= MUTE_FLOOR_DB { f32::NEG_INFINITY } else { v };
                    session.edit_silently(|p| {
                        if let Some(r) = p.region_mut(id) {
                            r.gain_db = Some(v);
                        }
                    });
                }
            });
        }
        widgets::row_separator(ui);
        widgets::row(ui, "Wygładzenie krawędzi", |ui| {
            ui.label(RichText::new("ms").font(theme::mono(15.0)).color(theme::TEXT_SECONDARY));
            let mut v = region.fade_ms.unwrap_or(track.duck.fade_ms);
            let r = ui.add(egui::DragValue::new(&mut v).range(0.0..=500.0).speed(0.5).fixed_decimals(0));
            if r.drag_started() {
                session.begin_gesture();
            }
            if r.changed() {
                session.edit_silently(|p| {
                    if let Some(r) = p.region_mut(id) {
                        r.fade_ms = Some(v);
                    }
                });
            }
            if region.fade_ms.is_some() && ui.small_button("domyślne").clicked() {
                session.edit(|p| {
                    if let Some(r) = p.region_mut(id) {
                        r.fade_ms = None;
                    }
                });
            }
        });
    });
}

/// Hides detections far from speech (e.g. breathing while listening).
fn discrimination_page(ui: &mut Ui, session: &mut Session, state: &mut UiState) {
    use oddech_core::Discrimination;
    header(
        ui,
        state,
        "Dyskryminacja",
        Some("Wykrycie się liczy, gdy w pobliżu słychać mowę. Pozostałe są ukryte i nie są ściszane."),
    );
    let d = session.project.discrimination;
    widgets::section_title(ui, "Odstęp od mowy");
    widgets::card(ui, |ui| {
        ui.add_space(8.0);
        let mut secs = f32::from(d.distance_s);
        let changed = slider_row(ui, session, &mut secs, 1.0..=f32::from(Discrimination::MAX_DISTANCE_S), 1.0, |v| format!("{v:.0} s"));
        if changed {
            session.edit_silently(|p| p.discrimination.distance_s = secs.round() as u8);
        }
        ui.label(RichText::new("Jak daleko od wykrycia szukać mowy.").size(14.0).color(theme::TEXT_SECONDARY));
        ui.add_space(6.0);
    });
    widgets::section_title(ui, "Próg");
    widgets::card(ui, |ui| {
        ui.add_space(8.0);
        let mut db = d.drop_db;
        let changed = slider_row(ui, session, &mut db, 0.0..=Discrimination::MAX_DROP_DB, 1.0, |v| format!("−{v:.0} dB"));
        if changed {
            session.edit_silently(|p| p.discrimination.drop_db = db.round());
        }
        ui.label(
            RichText::new("O ile ciszej niż zwykły poziom mowy może być dźwięk, który jeszcze uznajemy za mowę.")
                .size(14.0)
                .color(theme::TEXT_SECONDARY),
        );
        ui.add_space(6.0);
    });
    let hidden = session
        .project
        .regions
        .iter()
        .filter(|r| r.kind == oddech_core::RegionKind::Breath && !r.near_speech(&session.project.discrimination))
        .count();
    ui.add_space(10.0);
    ui.label(RichText::new(format!("Ukrytych wykryć: {hidden}")).size(14.0).color(theme::TEXT_TERTIARY));
}

/// A full-width slider with its value on the right; one undo step per drag.
fn slider_row(
    ui: &mut Ui,
    session: &mut Session,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    label: impl Fn(f32) -> String,
) -> bool {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(label(*value)).font(theme::mono(15.0)).color(theme::TEXT_SECONDARY));
        });
    });
    ui.spacing_mut().slider_width = ui.available_width();
    let r = ui.add(egui::Slider::new(value, range).show_value(false).step_by(step));
    if r.drag_started() {
        session.begin_gesture();
    }
    r.changed()
}

fn track_info_page(ui: &mut Ui, session: &mut Session, state: &mut UiState) {
    header(ui, state, "Ścieżki", None);
    let fps = session.info.video_fps.map_or("—".to_owned(), |f| format!("{f:.3} kl/s"));
    ui.label(
        RichText::new(format!("{} · {} · wideo {fps}", session.info.container, format_hms_millis(session.duration())))
            .color(theme::TEXT_SECONDARY),
    );
    let tracks: Vec<_> = session.tracks.iter().map(|t| t.info.clone()).collect();
    for info in tracks {
        widgets::section_title(ui, &info.display_name());
        let ts = session.project.track(info.id);
        widgets::card(ui, |ui| {
            let facts = [
                ("Strumień", format!("{}", info.id.0)),
                ("Kodek", info.codec.clone()),
                ("Kanały", format!("{} ({})", info.channels, info.channel_layout_label())),
                ("Próbkowanie", format!("{} Hz", info.sample_rate)),
                ("Język", info.language.clone().unwrap_or_else(|| "—".into())),
                ("Długość", format_hms_millis(info.duration_secs)),
            ];
            for (k, v) in facts {
                widgets::row(ui, k, |ui| {
                    ui.label(RichText::new(v).color(theme::TEXT_SECONDARY));
                });
            }
            widgets::row_separator(ui);
            widgets::row(ui, "Wygładzenie krawędzi", |ui| {
                ui.label(RichText::new("ms").color(theme::TEXT_SECONDARY));
                let mut v = ts.duck.fade_ms;
                if ui.add(egui::DragValue::new(&mut v).range(0.0..=500.0).speed(0.5).fixed_decimals(0)).changed() {
                    session.edit(|p| p.track_mut(info.id).duck.fade_ms = v);
                }
                if (ts.duck.fade_ms - DEFAULT_FADE_MS).abs() > f32::EPSILON && ui.small_button("domyślne").clicked() {
                    session.edit(|p| p.track_mut(info.id).duck.fade_ms = DEFAULT_FADE_MS);
                }
            });
        });
    }
}

fn timeline_page(ui: &mut Ui, session: &mut Session, settings: &mut Settings, state: &mut UiState, actions: &mut Vec<Action>) {
    header(
        ui,
        state,
        "Oś czasu do montażu",
        Some("Nagranie pocięte na granicach wdechów (nic nie jest usuwane); klipy wdechów są już ściszone i mają markery."),
    );
    widgets::section_title(ui, "Zapisz jako");
    let n = session.review_list().len();
    widgets::card(ui, |ui| {
        ui.add_enabled_ui(n > 0 && !state.busy, |ui| {
            if widgets::nav_row(ui, "FCPXML (DaVinci Resolve, Final Cut, Premiere)").clicked() {
                actions.push(Action::ExportTimeline);
            }
        });
    });
    widgets::section_title(ui, "Oś czasu w programie montażowym");
    widgets::card(ui, |ui| {
        widgets::row(ui, "Początek osi", |ui| {
            egui::ComboBox::from_id_salt("tc_start")
                .selected_text(format!("{:02}:00:00:00", settings.timeline_start_hours))
                .show_ui(ui, |ui| {
                    for h in [0u8, 1, 10] {
                        ui.selectable_value(&mut settings.timeline_start_hours, h, format!("{h:02}:00:00:00"));
                    }
                });
        });
    });
    ui.add_space(8.0);
    ui.label(
        RichText::new(format!(
            "Wdechy: {}. DaVinci Resolve zaczyna oś czasu od 01:00:00:00.",
            super::topbar::breath_count_label(n)
        ))
        .color(theme::TEXT_SECONDARY),
    );
    widgets::section_title(ui, "Import w DaVinci Resolve");
    ui.label(
        RichText::new(
            "1. File → Import → Timeline… (Ctrl+Shift+I) i wybierz plik .fcpxml. Powstaje nowa oś czasu z nagraniem pociętym na wdechach; głośność klipów wdechów zmienisz w inspektorze.\n\
             2. Żeby użyć tego na istniejącej osi czasu: przeciągnij zaimportowaną oś z Media Pool w miejsce klipu, prawy klik → Decompose in Place, usuń stary klip.\n\
             Między wdechami skaczesz Shift+↑ / Shift+↓ (markery).",
        )
        .size(14.0)
        .color(theme::TEXT_SECONDARY),
    );
}

fn training_page(ui: &mut Ui, session: &mut Session, settings: &mut Settings, state: &mut UiState, actions: &mut Vec<Action>) {
    header(ui, state, "Dane treningowe", Some("Twoje decyzje służą do oceny wykrywania i jako dane do ewentualnego dotrenowania modelu."));
    let regions = &session.project.regions;
    let user_breath = regions.iter().filter(|r| r.source == oddech_core::RegionSource::User && r.kind == oddech_core::RegionKind::Breath).count();
    let user_not = regions.iter().filter(|r| r.source == oddech_core::RegionSource::User && r.kind == oddech_core::RegionKind::NotBreath).count();
    let pending = regions.iter().filter(|r| r.is_pending()).count();
    let has_regions = !regions.is_empty();
    widgets::section_title(ui, "Adnotacje w tym projekcie");
    widgets::card(ui, |ui| {
        for (k, v) in [("Potwierdzone wdechy", user_breath), ("Odrzucone („to nie wdech”)", user_not), ("Nieprzejrzane", pending)] {
            widgets::row(ui, k, |ui| {
                ui.label(RichText::new(v.to_string()).font(theme::mono(15.0)).color(theme::TEXT_SECONDARY));
            });
        }
    });
    widgets::section_title(ui, "Jakość wykrywania");
    let threshold = settings.threshold;
    let q = oddech_core::quality::evaluate(&session.project.regions, threshold);
    let pct = |v: Option<f32>| v.map_or("—".to_owned(), |v| format!("{:.0}%", v * 100.0));
    widgets::card(ui, |ui| {
        if q.labelled() == 0 {
            ui.add_space(10.0);
            ui.label(
                RichText::new("Przejrzyj kilka wykrytych wdechów (Zatwierdź / To nie wdech), a tu pojawi się precyzja i czułość wykrywania.")
                    .color(theme::TEXT_SECONDARY),
            );
            ui.add_space(10.0);
            return;
        }
        let rows = [
            ("Precyzja (trafne / wykryte)", pct(q.precision())),
            ("Czułość (wykryte / wszystkie)", pct(q.recall())),
            ("F1", pct(q.f1())),
            ("Trafne · fałszywe · pominięte", format!("{} · {} · {}", q.true_pos, q.false_pos, q.false_neg)),
        ];
        for (k, v) in rows {
            widgets::row(ui, k, |ui| {
                ui.label(RichText::new(v).font(theme::mono(15.0)).color(theme::TEXT_SECONDARY));
            });
        }
        if let Some((t, best)) = oddech_core::quality::best_threshold(&session.project.regions)
            && (t - threshold).abs() > 0.01
            && best.f1() > q.f1()
        {
            widgets::row_separator(ui);
            ui.horizontal(|ui| {
                ui.set_min_height(40.0);
                let text = format!("Ustaw czułość {:.2} (F1 {})", 1.0 - t, pct(best.f1()));
                if ui.add(egui::Button::new(RichText::new(text).size(16.0).color(theme::ACCENT)).frame(false)).clicked() {
                    settings.threshold = t;
                }
            });
        }
    });
    ui.label(
        RichText::new(format!("Model: {}. Liczone na przejrzanych regionach tego nagrania.", crate::detection::DETECTOR_NAME))
            .size(13.0)
            .color(theme::TEXT_SECONDARY),
    );
    widgets::section_title(ui, "Eksport etykiet");
    widgets::card(ui, |ui| {
        ui.add_enabled_ui(has_regions && !state.busy, |ui| {
            if widgets::nav_row(ui, "CSV").clicked() {
                actions.push(Action::ExportLabels(LabelFormat::Csv));
            }
            widgets::row_separator(ui);
            if widgets::nav_row(ui, "JSON").clicked() {
                actions.push(Action::ExportLabels(LabelFormat::Json));
            }
        });
    });
}

/// Opens the file manager with `path` selected (or its folder). Runs off
/// the UI thread: the D-Bus call can wait for its timeout.
fn show_in_folder(path: &std::path::Path) {
    let path = path.to_owned();
    std::thread::spawn(move || reveal(&path));
}

fn reveal(path: &std::path::Path) {
    let uri = format!("file://{}", path.display());
    let selected = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.FileManager1",
            "--object-path",
            "/org/freedesktop/FileManager1",
            "--method",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("['{}']", uri.replace('\'', "%27")),
            "",
        ])
        .status()
        .is_ok_and(|s| s.success());
    if !selected {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        if let Err(e) = std::process::Command::new("xdg-open").arg(dir).spawn() {
            log::warn!("cannot open {}: {e}", dir.display());
        }
    }
}
