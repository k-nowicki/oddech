//! Help panel (F1): keyboard and mouse shortcuts, and how the program works
//! and what to expect from it. Available in every state of the window.

use egui::{Color32, CornerRadius, RichText, Stroke, Ui, vec2};

use super::{UiState, theme, widgets};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelpPage {
    #[default]
    Shortcuts,
    Guide,
}

/// Keys of one shortcut; alternatives are separate entries.
type Keys = &'static [&'static [&'static str]];

const SHORTCUTS: &[(&str, &[(Keys, &str)])] = &[
    (
        "Odtwarzanie",
        &[
            (&[&["Spacja"]], "odtwarzaj / zatrzymaj"),
            (&[&["L"]], "odtwarzaj"),
            (&[&["K"]], "zatrzymaj"),
            (&[&["J"]], "5 s wstecz"),
            (&[&["Enter"]], "odtwórz zaznaczenie lub wdech"),
            (&[&["Home"]], "na początek"),
            (&[&["End"]], "na koniec"),
        ],
    ),
    (
        "Przegląd wdechów",
        &[
            (&[&["N"]], "następny wdech"),
            (&[&["P"]], "poprzedni wdech"),
            (&[&["Z"]], "zatwierdź i przejdź dalej"),
            (&[&["X"]], "to nie wdech"),
            (&[&["B"]], "dodaj wdech z zaznaczenia"),
            (&[&["Delete"], &["Backspace"]], "usuń zaznaczony wdech"),
            (&[&["Esc"]], "odznacz, zamknij panel"),
        ],
    ),
    (
        "Edycja i plik",
        &[
            (&[&["Ctrl", "Z"]], "cofnij"),
            (&[&["Ctrl", "Shift", "Z"], &["Ctrl", "Y"]], "ponów"),
            (&[&["Ctrl", "O"]], "otwórz nagranie"),
            (&[&["F1"]], "pomoc"),
        ],
    ),
    (
        "Widok",
        &[
            (&[&["="]], "przybliż"),
            (&[&["-"]], "oddal"),
            (&[&["Shift", "Z"], &["\\"]], "całe nagranie w oknie"),
        ],
    ),
];

const MOUSE: &[(&str, &str)] = &[
    ("Kliknięcie na ścieżce lub linijce", "przesuwa głowicę"),
    ("Przeciąganie po linijce", "przewija odsłuch (scrub)"),
    ("Przeciąganie po pustym miejscu ścieżki", "zaznacza fragment"),
    ("Kliknięcie wdechu", "zaznacza go i ustawia głowicę przed nim"),
    ("Przeciąganie krawędzi wdechu", "zmienia jego granice"),
    ("Przeciąganie wdechu w pionie", "jego własne ściszenie (w dół ciszej; poniżej −60 dB wyciszenie)"),
    ("Kółko, Ctrl+kółko, gest szczypania", "zoom wokół kursora"),
    ("Shift+kółko, środkowy przycisk", "przewijanie w poziomie"),
    ("Alt+kółko, kółko nad nagłówkami", "przewijanie ścieżek w pionie"),
];

/// "Jak to działa": (heading, paragraphs).
const GUIDE: &[(&str, &[&str])] = &[
    (
        "Otwieranie nagrania",
        &[
            "Oddech otwiera pliki wideo z jedną lub kilkoma ścieżkami audio (MKV, MP4, MOV i inne). Przy pierwszym otwarciu dźwięk wszystkich ścieżek jest dekodowany do pamięci podręcznej (około 4 GB na godzinę przy trzech ścieżkach stereo), więc kolejne otwarcia są natychmiastowe. Oryginalny plik nigdy nie jest zmieniany.",
        ],
    ),
    (
        "Wykrywanie",
        &[
            "Włączenie „Popraw wdechy” na ścieżce uruchamia wykrywanie. Sieć neuronowa Respiro-en ocenia dla każdych 10 ms, jak bardzo dźwięk przypomina wdech; długie nagrania liczone są w trzyminutowych kawałkach. Półtorej godziny nagrania to kilkanaście sekund na karcie NVIDIA i od kilkudziesięciu sekund do kilku minut na procesorze. Pasek stanu na dole okna pokazuje, na czym działa wykrywanie.",
            "Czego się spodziewać: w polskiej mowie model znajduje około 9 na 10 wdechów. Pomija głównie wydechy. Zaznacza też czasem mlaśnięcia, stuki i chrząknięcia; to zwykle dźwięki, które i tak warto ściszyć. Każde wykrycie warto przejrzeć.",
        ],
    ),
    (
        "Czułość",
        &[
            "Każde wykrycie ma pewność modelu (widać ją w dymku po najechaniu na wdech). Czułość w „Więcej opcji” ustala, od jakiej pewności wykrycie liczy się jako wdech: wyższa daje więcej wykryć, także wątpliwych. Wykrycia poniżej progu są widoczne jako przerywane obramowanie; można je kliknąć i zatwierdzić (Z). Zmiana działa od razu, bez ponownego wykrywania, i dotyczy wszystkich nagrań.",
        ],
    ),
    (
        "Dyskryminacja",
        &[
            "Ukrywa wykrycia z dala od mowy, np. szmery w długich przerwach. Poziom mowy to głośność, którą ścieżka osiąga w najgłośniejszych 2% czasu. Wykrycie zostaje, jeśli w odległości „Odstęp od mowy” (domyślnie 10 s) jest dźwięk nie cichszy niż poziom mowy minus „Próg” (domyślnie 10 dB). Ukryte wykrycia nie są pokazywane, ściszane ani eksportowane. Ustawienie należy do nagrania.",
        ],
    ),
    (
        "Przegląd",
        &[
            "Pasek na dole okna prowadzi przez wdechy po kolei. „Zatwierdź” i „To nie wdech” rozstrzygają bieżący i przechodzą do następnego; odsłuch zaczyna się „Start przed wdechem” sekund wcześniej (albo tylko od wdechu, gdy włączone jest „Odtwarzaj tylko wdech”). Brakujący wdech: zaznacz go przeciągnięciem i kliknij „+ Dodaj wdech” (B). Granice poprawisz, przeciągając krawędź.",
            "„Zatwierdź wszystkie” zatwierdza pozostałe wdechy i zapisuje kopię referencyjną („<nagranie>.oddech.reference.json”): stały punkt odniesienia do pomiaru jakości wykrywania. Ponowne wykrywanie nie zmienia wdechów przejrzanych ani dodanych ręcznie.",
        ],
    ),
    (
        "Ściszanie",
        &[
            "Suwak „Ściszenie” na górze dotyczy wszystkich wdechów i jest pamiętany między nagraniami; do końca w lewo oznacza wyciszenie. Pojedynczy wdech ma własne ściszenie po przeciągnięciu go w pionie albo w „Więcej opcji → Zaznaczony wdech”, gdzie jest też wygładzenie krawędzi: krótkie przejście głośności, które zapobiega trzaskom. Jego domyślną wartość dla ścieżki (15 ms) zmienisz w „Więcej opcji → Informacje o ścieżkach”.",
            "„Przed / Po” przełącza odsłuch i przebieg między oryginałem a wersją ze ściszeniem; nie wpływa na eksport. M i S w nagłówku ścieżki (wyciszenie, solo) służą tylko do odsłuchu.",
        ],
    ),
    (
        "Eksport",
        &[
            "„Eksportuj” pyta, gdzie zapisać (domyślnie obok nagrania, z dopiskiem „-oddech”). Wideo: obraz i pozostałe ścieżki są kopiowane bez zmian, a ścieżki z „Popraw wdechy” ściszone i zakodowane tym samym kodekiem (FLAC, jeśli brak kodera). Ścieżki WAV: jeden plik na poprawianą ścieżkę. Format wybierasz w „Więcej opcji → Eksport”.",
            "Oś czasu FCPXML (Więcej opcji → Oś czasu do montażu) to pocięta oś czasu do DaVinci Resolve: wdechy są osobnymi klipami z ustawioną głośnością, więc w montażu można je jeszcze zmienić.",
        ],
    ),
    (
        "Zapisywanie",
        &[
            "Poprawki zapisują się same chwilę po każdej zmianie, w pliku „<nagranie>.oddech.json” obok nagrania. Ctrl+Z cofa do 200 kroków. Ustawienia ogólne (ściszenie, czułość, widok, eksport) są w ~/.config/oddech.",
        ],
    ),
    (
        "Gdy coś nie działa",
        &[
            "Dymek nad wskaźnikiem wykrywania w pasku stanu podaje kartę graficzną albo powód pracy na procesorze. Dziennik programu: ~/.local/state/oddech/oddech.log (poprzednie uruchomienie w oddech.log.1). Zajętość i limit pamięci podręcznej: „Więcej opcji → Pamięć podręczna”.",
        ],
    ),
];

/// Toggles the help panel (F1).
pub fn toggle(state: &mut UiState) {
    state.help_open = !state.help_open;
    if state.help_open {
        state.options_open = false;
    }
}

pub fn show(ui: &mut Ui, state: &mut UiState) {
    egui::Panel::right("help")
        .exact_size(460.0)
        .resizable(false)
        .frame(egui::Frame::new().fill(theme::BG_ELEVATED).inner_margin(egui::Margin::symmetric(28, 0)))
        .show(ui, |ui| {
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Pomoc").font(theme::bold(26.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::icon_button(ui, "✕", "Zamknij (F1, Esc)").clicked() {
                        state.help_open = false;
                    }
                });
            });
            ui.add_space(10.0);
            let mut guide = state.help_page == HelpPage::Guide;
            if widgets::segmented(ui, &mut guide, "Skróty", "Jak to działa") {
                state.help_page = if guide { HelpPage::Guide } else { HelpPage::Shortcuts };
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                match state.help_page {
                    HelpPage::Shortcuts => shortcuts(ui),
                    HelpPage::Guide => guide_page(ui),
                }
                ui.add_space(24.0);
            });
        });
}

fn shortcuts(ui: &mut Ui) {
    for (group, items) in SHORTCUTS {
        widgets::section_title(ui, group);
        widgets::card(ui, |ui| {
            for (i, (keys, what)) in items.iter().enumerate() {
                if i > 0 {
                    widgets::row_separator(ui);
                }
                ui.horizontal(|ui| {
                    ui.set_min_height(34.0);
                    for (j, combo) in keys.iter().enumerate() {
                        if j > 0 {
                            ui.label(RichText::new("lub").color(theme::TEXT_TERTIARY));
                        }
                        for (k, key) in combo.iter().enumerate() {
                            if k > 0 {
                                ui.label(RichText::new("+").color(theme::TEXT_TERTIARY));
                            }
                            keycap(ui, key);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(*what).size(14.0).color(theme::TEXT_SECONDARY)).truncate());
                    });
                });
            }
        });
    }
    widgets::section_title(ui, "Mysz");
    widgets::card(ui, |ui| {
        for (i, (gesture, what)) in MOUSE.iter().enumerate() {
            if i > 0 {
                widgets::row_separator(ui);
            }
            ui.add_space(6.0);
            ui.label(RichText::new(*gesture).size(15.0));
            ui.add(egui::Label::new(RichText::new(*what).size(14.0).color(theme::TEXT_SECONDARY)).wrap());
            ui.add_space(6.0);
        }
    });
}

fn keycap(ui: &mut Ui, key: &str) {
    let text = RichText::new(key).font(theme::mono(13.0)).color(theme::TEXT);
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
    let size = vec2((galley.size().x + 12.0).max(24.0), 22.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(5), theme::BG_CONTROL, Stroke::new(1.0, theme::SEPARATOR_CARD), egui::StrokeKind::Inside);
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

fn guide_page(ui: &mut Ui) {
    for (heading, paragraphs) in GUIDE {
        widgets::section_title(ui, heading);
        widgets::card(ui, |ui| {
            ui.add_space(8.0);
            for (i, p) in paragraphs.iter().enumerate() {
                if i > 0 {
                    ui.add_space(8.0);
                }
                ui.add(egui::Label::new(RichText::new(*p).size(14.5).color(theme::TEXT)).wrap());
            }
            ui.add_space(8.0);
        });
    }
}
