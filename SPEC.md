# Specyfikacja: Oddech, aplikacja do wykrywania i ściszania wdechów

## 1. Cel

Desktopowa aplikacja na Linuksa (X11 i Wayland), napisana w Ruście, do **wykrywania i ściszania głośnych wdechów** w mowie nagranej na ścieżkach audio plików wideo.

Wymagania kluczowe:
- dowolny plik wideo na wejściu, z kilkoma ścieżkami audio (mowa bywa na różnych ścieżkach),
- widok **wszystkich ścieżek naraz, każda z przebiegiem amplitudy** (spektrogram jako opcja),
- **zoomowalny timeline jak w DaVinci Resolve**, z głowicą i odtwarzaniem wybranego fragmentu,
- wybór, które ścieżki poprawiamy,
- wykrywanie wdechów odporne na mylenie ich z sybilantami („sz”, „s”, „cz”) i niezaśmiecone oddechami w długich przerwach bez mowy,
- ściszanie niezależne od wykrywania i regulowane w dowolnym momencie,
- płynność: szybki start, płynny zoom i przewijanie przy długich nagraniach.

## 2. Dane wejściowe i środowisko

- **System:** Linux, sesja X11 lub Wayland.
- **Typowy materiał:** nagrania 10–90 min, 2–6 ścieżek audio, 48 kHz, kontenery MKV/MP4/MOV.
- **Dowolna konfiguracja ścieżek:** aplikacja nie zakłada, która ścieżka zawiera mowę, ile jest ścieżek, jakie mają nazwy, kodeki, liczbę kanałów (mono, stereo, dual-mono, wielokanałowe) ani częstotliwość próbkowania (np. 44,1 kHz, 48 kHz). Nie ma żadnych reguł zależnych od nazw ścieżek.
- **Wybór ścieżek do analizy odbywa się w aplikacji:** przełącznik „Popraw wdechy” w nagłówku każdej ścieżki decyduje, które ścieżki są analizowane i ściszane. Wybór można zmienić w dowolnym momencie; włączenie ścieżki uruchamia dla niej detekcję (z cache, jeśli już była liczona), wyłączenie zachowuje jej regiony. Wybór zapisywany jest w projekcie. Przy pierwszym otwarciu pliku żadna ścieżka nie jest zaznaczona automatycznie na podstawie nazwy.

## 3. Stos technologiczny

| Obszar | Crate / narzędzie |
|---|---|
| GUI | `eframe` / `egui` (backend `wgpu`) |
| Dekodowanie i demux | `ffmpeg-next` (systemowe FFmpeg) |
| Odtwarzanie audio | `cpal` |
| FFT | `realfft` / `rustfft` |
| Równoległość | `rayon`, kanały `crossbeam-channel` |
| Okna dialogowe plików | `rfd` |
| Serializacja | `serde`, `serde_json` |
| Wykrywanie wdechów | model Respiro-en w ONNX, uruchamiany przez `ort` (ONNX Runtime); eksport modelu i pomiary w Pythonie w katalogu `training/` |
| Transkrypcja | `whisper-rs` (whisper.cpp z akceleracją GPU); nieużywana w wykrywaniu, przewidziana do cięcia powtórek; budowana tylko z funkcją `whisper` |

Uzasadnienia wyborów technicznych są w `DECISIONS.md`.

## 4. Rdzeń aplikacji

### 4.1 Wczytywanie pliku
- Otwarcie pliku przez dialog (przycisk „Otwórz…” i Ctrl+O), argument z linii poleceń albo drag & drop. Okna dialogowe są powiązane z oknem aplikacji; po ich zamknięciu aplikacja wraca na wierzch.
- Dowolny kontener i kodek obsługiwany przez FFmpeg.
- Wykrycie wszystkich strumieni audio, z indeksem, nazwą lub tagiem językowym, kodekiem, liczbą kanałów i częstotliwością próbkowania.
- Dekodowanie do `f32` w natywnej częstotliwości, w tle, z paskiem postępu. UI jest responsywne od razu.
- Kanały: domyślnie jedna linia na strumień (mix kanałów do mono tylko do wyświetlania), z opcją rozwinięcia strumienia wielokanałowego na osobne kanały. Ściszanie działa jednakowo na wszystkie kanały strumienia.

### 4.2 Widok ścieżek
- Wszystkie ścieżki jedna pod drugą, na wspólnej osi czasu.
- Domyślny widok ścieżki to **przebieg amplitudy w jednym kolorze**. Ścieżki bez poprawy są wyszarzone.
- **Spektrogram** jest opcją w „Więcej opcji” („Spektrogram zamiast przebiegu”), z ustawieniami: skala częstotliwości liniowa lub logarytmiczna (domyślnie logarytmiczna), zakres dB, mapa kolorów, rozmiar okna FFT.
- Nagłówek ścieżki zawiera nazwę, **Mute**, **Solo** i przełącznik „Popraw wdechy”.
- Przełącznik „Przed / Po” dotyczy jednocześnie odsłuchu i widoku: w trybie „Po” przebieg w miejscach wdechów jest pokazany po ściszeniu, a oryginał jako delikatny kontur.

### 4.2a Hierarchia interfejsu
Interfejs rozdziela to, co potrzebne zawsze, od tego, co przydaje się rzadko (wzorzec: aplikacje Apple).
- **Zawsze na ekranie:** „Otwórz…”, nazwa pliku i liczba wdechów, odtwarzanie z timecode, jeden suwak „Ściszenie”, „Przed / Po”, „Eksportuj”, ścieżki z nagłówkami, pływający pasek przeglądu wdechów (numer wdechu, poprzedni/następny, „To nie wdech”, „Zatwierdź”, „Zatwierdź wszystkie”; po decyzji automatyczne przejście do kolejnego wdechu). Pasek ma stały rozmiar i położenie niezależnie od wyświetlanego wdechu. Pasek stanu u dołu okna: czy wykrywanie działa na GPU, czy na CPU (szczegóły i powód w dymku) oraz „Pomoc (F1)”: panel ze skrótami klawiszowymi, gestami myszy i opisem działania funkcji, dostępny także przed otwarciem nagrania.
- **Kontekstowo:** po zaznaczeniu fragmentu na ścieżce z „Popraw wdechy” obok zaznaczenia pojawia się przycisk „+ Dodaj wdech”.
- **W „Więcej opcji” (menu „…”):** czułość, dyskryminacja (osobna karta) i ponowne wykrywanie, rozbieg i „Odtwarzaj tylko wdech”, własne ściszenie i wygładzenie krawędzi zaznaczonego wdechu, spektrogram, rozdzielanie kanałów stereo, informacje o ścieżkach, format eksportu, oś czasu do montażu, dane treningowe, dane nagrania (ścieżki plików z „Pokaż w folderze”), pamięć podręczna (zajętość, limit, czyszczenie).
- Niewidoczne w interfejsie: pewność modelu, statystyki, legendy, przyciski zoomu (zoom i przewijanie gestami myszy, sekcja 4.3).

### 4.3 Timeline i nawigacja (jak w DaVinci Resolve)
- Linijka z timecode (HH:MM:SS:ms oraz opcjonalnie klatki przy znanym FPS wideo).
- **Głowica (playhead):** kliknięcie na linijce lub ścieżce ustawia głowicę, przeciąganie ją przesuwa (scrub).
- **Zoom:** kółko myszy lub Ctrl+kółko, **wokół pozycji kursora**; zakres od całego pliku do poziomu pojedynczych milisekund; skróty `=`/`-` oraz „dopasuj do okna”.
- **Przewijanie w poziomie:** Shift+kółko, przeciąganie środkowym przyciskiem, pasek przewijania.
- **Zaznaczenie zakresu:** przeciągnięcie myszą; odtwarzanie tylko zaznaczenia, z opcją pętli.
- Automatyczne przewijanie widoku za głowicą podczas odtwarzania (włączane i wyłączane).

### 4.4 Odtwarzanie
- **Odtwarzacz zawsze odtwarza całe nagranie od pozycji głowicy** i gra dalej, aż do pauzy lub końca pliku. To zachowanie domyślne, także podczas przeglądu wdechów.
- Spacja: play / pause od głowicy. Enter: odtwórz zaznaczenie. Skróty J/K/L.
- Miks ścieżek zgodnie z Mute/Solo.
- Ściszenie wdechów nakładane w torze odtwarzania w czasie rzeczywistym (sekcja 6).
- Przełącznik A/B: oryginał kontra wersja ze ściszeniem, działający natychmiast podczas odtwarzania.
- Niskie opóźnienie; pozycja głowicy wynika z liczby faktycznie odtworzonych próbek (bez dryfu).
- Resampling do częstotliwości urządzenia, jeśli jest inna.

### 4.5 Regiony i adnotacje
- Region to fragment na konkretnej ścieżce z typem (`breath`, `not_breath`), źródłem (`model` lub `user`), pewnością (dla regionów z modelu) i opcjonalnymi parametrami ściszenia.
- Ręczne dodawanie regionów przez zaznaczenie na ścieżce.
- Edycja granic regionu przez przeciąganie krawędzi; usuwanie klawiszem Delete.
- Nawigacja: następny / poprzedni region (`N`/`P`).
- Zapis projektu do pliku obok wideo: regiony, decyzje przeglądu, nadpisania ściszenia, wygładzenie krawędzi, dyskryminacja, wybór ścieżek do poprawy. Ponowne otwarcie wideo wczytuje ten stan.
- „Zatwierdź wszystkie” zatwierdza wszystkie wykryte wdechy, zapisuje projekt i tworzy obok nagrania **kopię referencyjną tylko do odczytu**. Kopia referencyjna jest punktem odniesienia dla metryk jakości (6.5) i nie jest zmieniana przez wykrywanie.
- Wykrywanie nie zmienia regionów przejrzanych ani dodanych ręcznie: ponowne wykrywanie zastępuje tylko nieprzejrzane propozycje i nie dodaje nowych w miejscach już rozstrzygniętych.
- Eksport etykiet do CSV lub JSON (ścieżka, start, koniec, typ) jako **dane treningowe**.

### 4.6 Ustawienia i pliki
- **Ustawienia ogólne** (w katalogu konfiguracji XDG, zapisywane od razu po zmianie): ściszenie wdechów, czułość, rozbieg, widok, format eksportu, limit pamięci podręcznej, początek osi czasu w programie montażowym.
- **Ustawienia projektu** (obok nagrania): wybór ścieżek, mute/solo, wygładzenie krawędzi, dyskryminacja, regiony i ich nadpisania.
- **Katalog aplikacji:** model i biblioteki są częścią aplikacji i leżą obok programu; aplikacja działa z dowolnego katalogu i nie instaluje niczego w katalogu domowym.

## 5. Wydajność

Cel: **płynne 60 fps** przy zoomie i przewijaniu nagrania 90 min z 6 ścieżkami 48 kHz.

- Spektrogramy liczone w tle (`rayon`), w **piramidzie rozdzielczości** (jak mipmapy) i dzielone na kafelki; do GPU trafiają tylko kafelki widoczne w danym zoomie.
- Waveform z piramidy min/max.
- Cache wyników na dysku, dzięki któremu drugie otwarcie pliku jest natychmiastowe. Limit rozmiaru jest ustawieniem (domyślnie 20 GB); po jego przekroczeniu usuwane są najdawniej otwierane nagrania, nigdy otwarte.
- Budżet pamięci ustalony i opisany w `DECISIONS.md`; w RAM tylko to, co potrzebne do bieżącego widoku i odtwarzania.
- Dekodowanie, FFT i detekcja działają w wątkach roboczych; wątek UI pozostaje płynny.

## 6. Detekcja wdechów

### 6.1 Zasada działania
Wdech i sybilant („sz”) są akustycznie podobne; o ich rozróżnieniu decyduje model uczony na mowie. Detekcja składa się z trzech etapów:

1. **Model wykrywania wdechów** (Respiro-en) ocenia każdą klatkę 10 ms dźwięku ścieżki oznaczonej „do poprawy”. Długie nagrania są przetwarzane w oknach 3 min, każde z własnym odniesieniem poziomu, co kompensuje zmiany odległości od mikrofonu.
2. **Segmenty:** ciągi klatek wdechu stają się regionami; pewność regionu to najwyższe prawdopodobieństwo w segmencie.
3. **Dyskryminacja:** wykrycie się liczy tylko wtedy, gdy w pobliżu słychać mowę (6.2a). Wyklucza to oddechy w długich przerwach bez mowy.

Etykieta „wdech” obejmuje także mlaśnięcia, stuki i chrząknięcia w przerwach; są ściszane tak samo jak wdechy. Wydechy przeciągnięte po spółgłosce bywają pomijane i dodaje się je ręcznie.

### 6.2 Model
- Respiro-en wyeksportowany do ONNX i uruchamiany przez `ort` na GPU (CUDA) albo CPU (§10). Cechy wejściowe (spektrogram melowy, jego wariancja, ZCR) liczone w Ruście zgodnie z implementacją referencyjną (test porównawczy).
- Każdy wykryty region ma wartość pewności (0–1).
- Dotrenowany model (na kopiach referencyjnych) może zastąpić obecny tylko wtedy, gdy na nagraniach niewidzianych przy treningu osiąga lepsze metryki (6.5).

### 6.2a Dyskryminacja
- **Poziom mowy** ścieżki: głośność, którą przekracza tylko 2% najgłośniejszych klatek (25 ms co 10 ms).
- Wykrycie się liczy, jeśli w odległości ±X s od niego jest jakikolwiek dźwięk nie ciszej niż poziom mowy − D dB.
- Suwaki w karcie „Dyskryminacja”: odstęp X od 1 do 30 s (domyślnie 10 s), próg D od 0 do 30 dB (domyślnie 10 dB). Działają natychmiast, bez ponownej analizy; ustawienie należy do projektu.
- Wykrycia odrzucone przez dyskryminację nie są rysowane, ściszane ani eksportowane.

### 6.3 Przegląd wyników w UI
- Wykryte wdechy jako regiony na spektrogramie odpowiedniej ścieżki; pewność widoczna jako intensywność koloru.
- Szybki przegląd z klawiatury: następny kandydat, odtwórz, **akceptuj**, **odrzuć**, skoryguj granice.
- Przejście do wdechu (następny / poprzedni, kliknięcie regionu, automatyczne przejście po decyzji) **tylko przesuwa głowicę** na początek wdechu minus rozbieg. Odtwarzanie działa dalej normalnie, czyli przez wdech i dalej przez nagranie.
- **Rozbieg** to ustawienie „Start przed wdechem” w sekcji „Więcej opcji”: pole liczbowe w sekundach, minimum 0 (0 oznacza start dokładnie na wdechu), z dokładnością do 0,1 s; domyślnie 2 s. Wartość jest zapamiętywana między uruchomieniami aplikacji.
- Opcja (domyślnie wyłączona): „Odtwarzaj tylko wdech”, czyli odtwarzanie samego regionu.
- Suwak progu pewności („Czułość”) z natychmiastowym podglądem, które regiony są aktywne. Regiony poniżej progu są widoczne tylko jako blady przerywany kontur.
- Każda decyzja przeglądu jest zapisywana jako adnotacja (dane do metryk i ewentualnego dotrenowania).

### 6.4 Ściszanie: niedestrukcyjne i niezależne od detekcji
Detekcja i ściszanie to **dwie rozdzielone warstwy**:

1. **Warstwa oznaczeń:** regiony z wykrywania i dodane ręcznie.
2. **Warstwa ściszania:** parametry ściszenia przechowywane osobno i **zmieniane w dowolnym momencie po oznaczeniu**, bez ponownego uruchamiania detekcji.

Wymagania:
- **Globalna siła ściszenia**, w dB, jedna dla wszystkich ścieżek i nagrań (ustawienie ogólne), regulowana suwakiem w zakresie od 0 dB do pełnego wyciszenia; domyślnie −18 dB.
- **Nadpisanie per region:** dowolny region może mieć własną wartość ściszenia, zmienianą przeciąganiem w pionie na regionie albo w panelu właściwości; region bez nadpisania dziedziczy wartość globalną.
- **Fade in i fade out** na krawędziach regionu: per ścieżka w projekcie, domyślnie 15 ms, z możliwością nadpisania per region.
- Ściszanie realizowane jako obwiednia gain nakładana w czasie rzeczywistym w torze odtwarzania; każda zmiana parametrów jest słyszalna od razu.
- Audio źródłowe pozostaje w pamięci w oryginalnej postaci; ściszenie jest wypalane do plików dopiero przy eksporcie.
- Ściszanie działa na ścieżkach oznaczonych „do poprawy”.

### 6.5 Metryka jakości
Na nagraniach z kopią referencyjną skrypt `training/eval_respiro.py` raportuje precision, recall i błąd granic wykrycia; aplikacja pokazuje precision i recall na przejrzanych regionach bieżącego nagrania (karta „Dane treningowe”). Wynik sprawdzany po każdej zmianie modelu lub parametrów wykrywania.

## 7. Eksport

- **Poprawione ścieżki jako WAV:** osobno każda ścieżka oznaczona „do poprawy”, w natywnej częstotliwości, float 32 lub 24-bit.
- **Remux:** nowy plik wideo z podmienionymi ścieżkami audio, obraz kopiowany bez ponownego kodowania (stream copy), z zachowaniem pozostałych ścieżek i metadanych.
- **Oś czasu do montażu (FCPXML):** oryginalne nagranie pocięte na granicach wdechów (nic nie jest usuwane), klipy wdechów z ustawioną głośnością (ściszenie z aplikacji) i markerem. Początek osi czasu ustawialny (domyślnie 01:00:00:00).
- Każdy eksport pyta o miejsce zapisu. Domyślnie proponuje katalog nagrania i nazwę `<nazwa>-oddech.<ext>`; zapis w miejsce oryginału jest niemożliwy.

## 8. Kryteria akceptacji

**M1: Wczytanie i widok**
- Otwiera plik MKV lub MP4 z kilkoma ścieżkami audio; pokazuje wszystkie ścieżki ze spektrogramami.
- Zoom wokół kursora, przewijanie, linijka z timecode.
- Test: plik syntetyczny wygenerowany FFmpegiem (np. 3 ścieżki z różnymi tonami i szumem) otwiera się i wyświetla poprawnie; renderowany zrzut spektrogramu zgadza się z oczekiwaniami.

**M2: Odtwarzanie**
- Głowica, scrub, play/pause, odtwarzanie zaznaczenia, pętla, Mute/Solo.
- Brak dryfu głowicy po 10 minutach odtwarzania (weryfikacja przez licznik próbek).

**M3: Wydajność i cache**
- Płynny zoom i przewijanie na nagraniu 90 min z 6 ścieżkami; drugie otwarcie z cache w mniej niż 2 s.

**M4: Regiony i ściszanie**
- Ręczne regiony, edycja granic, zapis i odczyt projektu, eksport etykiet.
- Ściszanie globalne i per region słyszalne od razu w odtwarzaniu, przełącznik A/B; bufory źródłowe identyczne przed i po zmianach (test).

**M5: Detekcja**
- Model wykrywania wdechów per ścieżka, dyskryminacja z suwakami, przegląd akceptuj/odrzuć, kopie referencyjne, raport precision i recall.
- Test: cechy wejściowe modelu liczone w Ruście zgodne z implementacją referencyjną; wyniki ONNX zgodne z PyTorchem.

**M6: Eksport**
- WAV per ścieżka, remux ze stream copy obrazu, oś czasu FCPXML pocięta na wdechach.
- Test: wyeksportowany WAV zgadza się próbka w próbkę z wynikiem toru odtwarzania dla tych samych parametrów.

## 9. Standard jakości

Aplikacja jest narzędziem indywidualnym, ale kod ma być klasy produkcyjnej: czytelny podział na moduły, przemyślane granice między warstwami (dekodowanie, analiza, detekcja, ściszanie, odtwarzanie, UI), obsługa błędów, testy i spójny styl. Istotne decyzje techniczne i ich uzasadnienia są zapisane w `DECISIONS.md`.

## 10. Akceleracja i środowisko docelowe

- **GPU: NVIDIA, CUDA.** Model wykrywania wdechów (`ort` z CUDA Execution Provider) liczony na GPU, gdy przejdzie test zgodności z CPU; w przeciwnym razie na CPU. Dziennik podaje urządzenie i powód pracy na CPU.
- Instrukcja budowania dla środowiska docelowego jest w `README.md`.

## 11. Materiały referencyjne

Ścieżki względem katalogu projektu.

- **Makiety GUI:**
  - `Mockups/main-window.png`: główne okno (ścieżki z przebiegiem amplitudy, górny pasek, pasek przeglądu wdechów).
  - `Mockups/more-options.png`: panel „Więcej opcji” otwierany z menu „…”.

Makiety są wiążące co do układu, hierarchii i zachowania interfejsu (sekcje 4.2a, 4.4, 6.3), z uzupełnieniami opisanymi w 4.2a. Szczegóły wizualne (dokładne kolory, fonty, odstępy) mogą odbiegać od makiet w granicach możliwości egui. Dane widoczne na makietach (nazwa pliku, liczby, czasy) są przykładowe.
