# Decyzje techniczne

Istotne decyzje techniczne i ich uzasadnienia. Wymagania opisuje `SPEC.md`, budowanie i użytkowanie `README.md`.

## Środowisko docelowe

- Maszyna deweloperska: Ubuntu 24.04, sesja X11, NVIDIA RTX 4060 Ti 16 GB, sterownik 580, systemowe CUDA 12.0 (`nvidia-cuda-toolkit`), cuDNN 9, FFmpeg 6.1, GCC 13 (domyślny) i GCC 12, 20 rdzeni, 62 GB RAM.
- Rust 1.98 instalowany przez rustup w katalogu domowym.
- Dźwięk: ALSA (przez PipeWire/PulseAudio) — `cpal` z domyślnym hostem ALSA.
- X11 i Wayland: `eframe` z obiema funkcjami; winit wybiera Wayland, gdy jest `WAYLAND_DISPLAY`. `libwayland` ładowana w czasie działania (`winit/wayland-dlopen`), żeby program startował też bez niej. Na Waylandzie ramkę okna rysuje aplikacja (GNOME nie robi tego za klientów) — w stylu Adwaita (`winit/wayland-csd-adwaita`), bez niej winit rysuje surowy szary pasek bez tytułu.
- Okna dialogowe (portal plików przez `rfd`) są przypięte do okna programu: na X11 przez identyfikator okna, na Waylandzie przez uchwyt eksportowany protokołem xdg-foreign. Do wątku dialogu trafiają surowe uchwyty okna i połączenia z serwerem wyświetlania.
- Przeciąganie plików do okna działa tylko na X11: winit 0.30 nie obsługuje go na Waylandzie.
- Rysowanie: wgpu próbuje Vulkana, a bez niego OpenGL (EGL). Na sterownikach Mesa (Intel, AMD, rysowanie programowe llvmpipe) przejście na OpenGL działa samo. Z EGL sterownika NVIDIA wgpu nie tworzy powierzchni na X11 („gl not compatible with provided surface”), ale sterownik NVIDIA zawsze dostarcza Vulkana; potrzebny jest tylko loader `libvulkan1`, więc paczka musi od niego zależeć. Bez żadnego działającego backendu program kończy się błędem zapisanym w dzienniku.
- Sprawdzone w zagnieżdżonej sesji GNOME 46 (Wayland): okno, ramka, rysowanie na GPU, pasek stanu, dialog otwierania.

## Architektura

Workspace z pięcioma crate'ami, z zależnościami płynącymi w jedną stronę:

| Crate | Rola | Zależy od |
|---|---|---|
| `oddech-core` | model domeny, DSP, cache, projekt; czysty Rust, bez I/O urządzeń | — |
| `oddech-media` | FFmpeg: probe, dekodowanie do cache, eksport (WAV, remux) | core |
| `oddech-playback` | silnik odtwarzania (cpal) | core |
| `oddech-detect` | wykrywanie wdechów (Respiro-en przez ONNX Runtime), poziom mowy do dyskryminacji; transkrypcja (whisper), nieużywana w wykrywaniu | core |
| `oddech` | aplikacja egui | wszystkie |

Uzasadnienie: warstwy (dekodowanie, analiza, detekcja, ściszanie, odtwarzanie, UI) mają twarde granice na poziomie kompilacji; ciężkie zależności (FFmpeg, CUDA) nie spowalniają testów rdzenia.

## Przechowywanie audio i budżet pamięci

- Zdekodowane audio trafia do cache na dysku jako jeden plik `f32` na kanał i jest **mapowane w pamięci** (`memmap2`). Najgorszy przypadek ze specyfikacji (90 min × 6 ścieżek stereo × 48 kHz) to ok. 12 GB próbek — nie mieści się sensownie w RAM, a mmap oddaje decyzję o rezydencji jądru (page cache). Dane źródłowe są tylko do odczytu, więc bufor źródłowy nie może zostać zmieniony (wymóg M4).
- W RAM na stałe trzymane są tylko piramidy przebiegu (min/max, `i16`, kubełek 64 próbki, współczynnik 4 między poziomami): ok. 21 MB na ścieżkę przy 90 min / 48 kHz, czyli ~130 MB dla 6 ścieżek.
- Piramidy per kanał (widok „Rozdziel kanały stereo”) liczone są dopiero na żądanie i zapisywane w cache.
- Spektrogram: kafelki 256×256 RGBA liczone na żądanie, trzymane jako tekstury GPU z LRU (limit 384 kafelki ≈ 100 MB VRAM). Kafelków spektrogramu nie zapisujemy na dysk — liczą się w milisekundach tylko dla widocznego fragmentu, a zmiana ustawień (FFT, skala, zakres dB, mapa kolorów) i tak unieważnia obrazy.
- Budżet RAM aplikacji (bez page cache): < 400 MB przy 90 min × 6 ścieżek.

## Cache

- Jeden katalog na nagranie, nazwany odciskiem pliku = BLAKE3(wersja formatu, rozmiar, mtime, pierwszy i ostatni 1 MiB). Hashowanie całego pliku wielogigabajtowego kosztowałoby sekundy i zniweczyło cel „drugie otwarcie < 2 s”.
- `manifest.json` zapisywany jest na końcu (atomowy rename) i oznacza kompletny wpis; przerwany import nigdy nie wygląda na kompletny.
- Leży w katalogu cache XDG, a nie w katalogu aplikacji: katalog aplikacji po instalacji bywa tylko do odczytu i wspólny dla wszystkich kont, a cache XDG to standardowe miejsce na dane, które można odtworzyć (pomijane przez kopie zapasowe).
- Rozmiar: ok. 4 GB na godzinę przy 3 ścieżkach stereo (f32, natywna częstotliwość), więc cache ma limit. Sprzątanie usuwa najdawniej otwierane wpisy (znacznik `last_used` aktualizowany przy każdym otwarciu) po otwarciu nagrania i po obniżeniu limitu; wpis otwartego nagrania nigdy nie jest usuwany.

## Dekodowanie

- Jeden przebieg demuksera, osobny wątek dekodera dla każdej ścieżki (kanał `crossbeam` z pakietami). Próbka 98 s × 3 ścieżki FLAC: 114 ms.
- Konwersja formatów próbek FFmpeg → `f32` planarnie własnym kodem (bez swresample): prostsza obsługa układów kanałów i brak zależności od API layoutów, które zmieniło się między FFmpeg 5/6/7.
- Wyrównanie ścieżek: próbka 0 każdej ścieżki odpowiada `start_time` kontenera; późniejszy start strumienia jest dopełniany ciszą, ujemny (priming) — obcinany.

## Odtwarzanie

- Wątek „feeder” renderuje bloki 256 ramek do lock-free kolejki (`rtrb`, 3072 ramki ≈ 64 ms), callback audio tylko kopiuje. Zmiana parametrów (A/B, ściszenie, mute/solo) jest słyszalna w ciągu długości kolejki.
- Każdy blok niesie znacznik pozycji na osi czasu; głowica = znacznik + liczba próbek faktycznie oddanych urządzeniu. Brak zegarów, brak dryfu (test: 10 min, pozycja zgodna co do próbki w każdym callbacku).
- Seek/stop: licznik generacji; callback porzuca stare dane i potwierdza.
- Resampling do częstotliwości urządzenia: własny **bezstanowy** interpolator windowed-sinc (32 tapy, okno Kaisera, tabela 512 faz). Ponieważ źródło jest dostępne swobodnie (mmap), nie potrzeba strumieniowego resamplera ze stanem — seek, pętla i scrub nie wymagają resetów. `rubato` 5 odrzucone jako nadmiarowe dla tego modelu.
- Ściszenie nakładane jest **przed** resamplingiem, w natywnej częstotliwości ścieżki, tą samą funkcją (`render_source_block`) co w eksporcie — dzięki temu eksport jest identyczny próbka w próbkę z tym, co słychać (test M6).

## Ściszanie

- Obwiednia: pełne ściszenie w całym regionie, rampy raised-cosine **na zewnątrz** regionu (przed startem i po końcu), symetryczne. Dzięki temu cały oznaczony wdech jest ściszony do zadanej głębokości.
- Nakładające się regiony: minimum wzmocnień.
- Suwak od 0 dB do −60 dB; −60 dB oznacza pełne wyciszenie (w JSON zapisywane jako `"mute"`).
- Ściszenie i czułość są ustawieniami ogólnymi, a nie projektu: siła ściszenia wynika ze stylu montażu, a nie z konkretnego nagrania, i jest praktycznie stała między nagraniami. Dyskryminacja zależy od warunków nagrania (przerwy, poziom tła), więc należy do projektu.
- Domyślny próg pewności 0,5: pewności Respiro skupiają się blisko 0 i 1, więc próg w środku zakresu oddziela je stabilnie.

## Eksport

- WAV: `hound`, float 32 lub 24-bit int, natywna częstotliwość, wszystkie kanały.
- Remux: wideo i pozostałe strumienie kopiowane bez rekodowania (stream copy), metadane i dyspozycje strumieni zachowane. Poprawiane ścieżki kodowane **tym samym kodekiem**, jeśli FFmpeg ma koder (FLAC, PCM, AAC, Opus, MP3, Vorbis, ALAC); w przeciwnym razie FLAC. Dla kodeków bezstratnych głębia bitowa jak w źródle — próbki poza wdechami są identyczne bit w bit (test). Dla stratnych bitrate nie niższy niż źródłowy. Zapis do pliku tymczasowego i atomowy rename.
- Pliki WAV dostają nazwy `<nazwa>-oddech-<ścieżka>.wav`, przy kolizji `(2)`, `(3)`… (wybiera się katalog, nie plik, więc kolizje rozwiązuje aplikacja).
- FCPXML 1.9: czasy wyrównane do siatki klatek, poprawne ułamki NTSC. Głośność klipu wdechu to `adjust-volume`, najwyżej −96 dB (FCPXML nie zapisze pełnego wyciszenia); obejmuje wszystkie ścieżki audio klipu, bo FCPXML nie ma prostego sposobu na ściszenie jednej. Import sprawdzony w DaVinci Resolve.
- Oś czasu zaczyna się domyślnie od 01:00:00:00, bo tak zaczynają się osie w Resolve; elementy przed początkiem osi są przy imporcie pomijane.
- EDL nie jest obsługiwany: dopasowuje źródło tylko po nazwie taśmy i kodzie czasowym, więc przy nagraniach bez własnego kodu czasowego (np. z OBS, wszystkie od 00:00:00:00) Resolve wybiera złe klipy. Nie przenosi też głośności ani wielu ścieżek audio.

## UI

- `eframe`/`egui` 0.36, backend `wgpu`. Czcionki DejaVu (systemowe) — zgodne z makietami i z pełnym pokryciem polskich znaków; fallback na wbudowane czcionki egui.
- Przebieg rysowany jako jedna siatka trójkątów na ścieżkę (kolumna = piksel fizyczny), kolor per wierzchołek pozwala pokazać ściszone fragmenty w kolorze akcentu; oryginał — przerywany kontur. Przy bardzo dużym zoomie rysowane są pojedyncze próbki.
- Odświeżanie: egui buduje i rysuje całe okno w każdej klatce. Bez interakcji okno odświeża się tylko przy zmianie; podczas odtwarzania 30 klatek/s, przy zadaniach w tle 10/s. Częstotliwość ekranu nie jest potrzebna, bo sam z siebie przesuwa się tylko wskaźnik odtwarzania, a przy rysowaniu programowym (Mesa llvmpipe/lavapipe, bez GPU) nie ma vsync i klatki szłyby bez limitu. Pomiar (okno 1440×860, odtwarzanie, 2072 wdechy): GPU 14% → 5% rdzenia, rysowanie programowe ~650% → ~150%. Pozostały koszt wynika z trybu natychmiastowego egui (całe okno w każdej klatce, bez przerysowywania fragmentów): bez GPU samo okno bez osi czasu to ~18 ms procesora na klatkę. Zmiana biblioteki interfejsu nie jest planowana, dopóki docelowe komputery mają działającą grafikę.
- Wdechy węższe niż 4 punkty rysowane są jako scalone pasy (sąsiednie w odległości do 1 px łączą się), bez zaokrągleń i etykiet; etykieta ściszenia pojawia się od 36 punktów szerokości. W pełnym widoku długiego nagrania pojedyncze wdechy i tak się nie rozróżniają, a rysowanie każdego osobno kosztowało więcej niż reszta osi czasu.
- Automatyczna skala wyświetlania per ścieżka (99,5 percentyl szczytów), żeby ciche ścieżki były czytelne; nie wpływa na dźwięk.
- Zoom kółkiem (także Ctrl+kółko) wokół kursora, Shift+kółko i środkowy przycisk — przewijanie; obsługa na surowych zdarzeniach kółka, bo egui domyślnie zamienia Ctrl+kółko na zoom interfejsu.
- Pomoc (F1, „Pomoc” w pasku stanu, wiersz w „Więcej opcji”) jest osobnym panelem po prawej, dostępnym także przed otwarciem nagrania. Zawiera jedyną listę skrótów klawiszowych i gestów myszy oraz opis działania funkcji; teksty są w `ui/help.rs`, obok kodu, który opisują.
- J cofa o 5 s zamiast odtwarzać wstecz: odtwarzanie wstecz nie jest obsługiwane.
- Cofanie (Ctrl+Z): migawki projektu, limit 200.
- Pasek stanu u dołu okna pokazuje, gdzie działa wykrywanie (kropka: niebieska GPU, szara CPU bez karty NVIDIA, żółta CPU po awarii GPU, czerwona brak modelu); powód i ścieżka dziennika w dymku. Model ładuje się w tle przy starcie programu, żeby wskaźnik był znany od razu, a pierwsze wykrywanie nie czekało na start GPU. Informacja nie trafia do „Dane nagrania”, bo dotyczy komputera, nie nagrania.
- Dziennik trafia do terminala i do pliku w katalogu stanu (XDG), żeby program uruchomiony z menu też zostawiał zapis; poprzednie uruchomienie zostaje jako `oddech.log.1`. Paniki są zapisywane w dzienniku.

## Projekt

- Zapis atomowy, automatyczny po 0,8 s od ostatniej zmiany i przy zamknięciu. Nieczytelny plik projektu nie jest nadpisywany — zostaje przemianowany na `.unreadable`.
- Kopia referencyjna jest osobnym plikiem tylko do odczytu, bo projekt zmienia się przy każdym wykrywaniu i edycji, a metryki potrzebują stałego punktu odniesienia. Jest podmieniana atomowo (plik tymczasowy + rename, więc prawa tylko do odczytu nie przeszkadzają).
- Nowe wykrycie jest pomijane, jeśli nachodzi na region przejrzany lub dodany ręcznie o ponad połowę krótszego z nich.
- Stare pliki z polem `threshold` (przed przeniesieniem czułości do ustawień ogólnych) wczytują się bez błędu.
- Ustawienia ogólne zapisujemy od razu, a nie przy zamknięciu okna, bo zamknięcie przez Ctrl+C w terminalu gubiło zmiany.

## CUDA i GPU

- whisper.cpp (przez `whisper-rs` z funkcją `cuda`, tylko przy funkcji kompilacji `whisper`) budowany z systemowym nvcc 12.0. nvcc 12.0 nie wspiera GCC 13, dlatego w `.cargo/config.toml` ustawiamy `CMAKE_CUDA_HOST_COMPILER=/usr/bin/g++-12`; architektura tylko `sm_89` (RTX 4060 Ti), co skraca kompilację.
- ONNX Runtime 1.22 (GPU) ładowany dynamicznie (`ort` 2.0.0-rc.13, funkcja `load-dynamic`) z katalogu aplikacji. Na tej maszynie biblioteki pochodzą z oficjalnej paczki `onnxruntime-linux-x64-gpu-1.22.0` (skopiowane do `dist/lib`); korzystają z systemowego CUDA 12 i cuDNN 9.
- `ort` budowany z `api-20`, nie `api-22`: z `api-22` ORT 1.22 przerywa program asercją przy tworzeniu sesji bez CUDA.
- Wybór urządzenia: CUDA jest próbowane tylko wtedy, gdy jest sterownik NVIDIA (`/dev/nvidiactl`), `CUDA_VISIBLE_DEVICES` nie ukrywa kart i obok `libonnxruntime.so` leży jego `libonnxruntime_providers_cuda.so`. ORT przy braku urządzenia przerywa proces zamiast zwrócić błąd, a bez własnego providera ładuje systemową kopię, która w innej wersji powoduje segfault.
- Test GPU przy ładowaniu: 1 s syntetycznego dźwięku liczona na GPU i CPU; różnica powyżej 0,01 albo błąd oznacza pracę na CPU. Część błędów pojawia się dopiero przy liczeniu, np. `cudaErrorNoKernelImageForDevice`, gdy paczka ORT nie zawiera kodu dla danej generacji kart (ORT 1.23.0 na RTX 50, microsoft/onnxruntime#26181). Błąd GPU w trakcie wykrywania przenosi model na CPU i powtarza okno.
- Urządzenie i powód pracy na CPU trafiają do dziennika jednym wierszem (`breath detection on GPU (…)` / `on CPU (…)`), z nazwą karty, wersją sterownika i skróconym komunikatem ORT (np. `CUDA failure 100: no CUDA-capable device is detected`), który da się wyszukać; pełny komunikat na poziomie `debug`.
- Model ładowany raz na proces, a sesje ORT nigdy nie są zwalniane: w ORT 1.22 zwolnienie sesji CPU przy istniejącej sesji CUDA uszkadza stertę (`corrupted double-linked list` przy wyjściu).
- Środowisko ORT nigdy nie jest zwalniane (jedna referencja celowo „wycieka”). `ort` zwalnia je w procedurze wyjścia, która działa po finalizacji dynamicznie załadowanej biblioteki, co kończyło się segfaultem przy zamykaniu programu.
- Czas: 104 min nagrania to 16 s na RTX 4060 Ti. CPU jest ok. 2 razy wolniejszy, więc też wystarcza.

## Wykrywanie wdechów

**Respiro-en** (Yang i in., INTERSPEECH 2024; licencja MIT; https://github.com/ydqmkkx/Respiro-en), trenowany na angielskich audiobookach (LibriTTS-R):
- Na 55 ręcznie oznaczonych wdechach (polska mowa, mikrofon pojemnościowy): precyzja 80%, czułość 89%; wszystkie pominięcia to wydechy. Dla porównania podejście oparte na transkrypcji (Whisper, kandydaci w przerwach między słowami, heurystyka) osiągnęło 53% i 82%, bo czasy słów z Whispera bywają przesunięte o 0,5 s.
- Model nie jest dotrenowywany: przy kilkudziesięciu przykładach ryzyko pogorszenia przewyższa możliwy zysk.
- Uruchamianie: eksport do ONNX (wyniki zgodne z PyTorchem do ~1e-5). Cechy (log-mel, wariancja, ZCR) policzone w Ruście tak samo jak w librosa; sprawdza to test z fixture.
- Okna 3 min z połową zakładki (każda klatka brana z okna, w którym leży najbliżej środka), bo pamięć uwagi modelu rośnie z kwadratem długości wejścia. Odniesienie poziomu per okno nadąża za zmianą odległości od mikrofonu; wspólne odniesienie dla całego nagrania daje praktycznie te same wyniki. Przesunięcie podziału o 45 s zmienia ~5% wykryć i nie przy granicach okien.
- Segmenty: klatki z prawdopodobieństwem > 0,064 (próg autorów), ciągi dłuższe niż 20 klatek (jak w implementacji referencyjnej).
- Wejście modelu to 16 kHz mono (miks kanałów ścieżki), przeliczane naszym interpolatorem sinc. Wyniki aplikacji zgadzają się ze skryptem w Pythonie (40/40 i 2042/2054 segmentów z identycznymi granicami; różnice to zaokrąglenia na GPU przy progu).
- Whisper zostaje w kodzie (przyda się przy cięciu powtórek), ale nie jest używany do wykrywania; wybór modelu mowy zniknął z interfejsu. Jest budowany tylko z funkcją `whisper` (domyślnie wyłączoną), bo whisper.cpp z CUDA wiąże program z bibliotekami CUDA już przy starcie — bez nich program by się nie uruchomił.
- YAMNet (planowany w SPEC jako dodatkowy sygnał) odrzucony przed wdrożeniem: ocenia dźwięk w oknach ~1 s, a wdechy trwają 0,2–1,3 s.
- `training/`: `eval_respiro.py` (pomiar względem kopii referencyjnych), `export_respiro_onnx.py` (eksport modelu i fixture do testu cech), `respiro_preview.py` (podgląd wykryć jako osobny projekt, bez zmiany istniejącego), `audibility.py` (eksperymenty z poziomem mowy).

## Dyskryminacja

W długich przerwach bez mowy (słuchanie, oglądanie) model poprawnie znajduje ciche oddechy, których nie ma sensu oznaczać.
- Poziom mowy jako 98. percentyl klatek RMS: to „powtarzający się maksymalny poziom bez pojedynczych skoków”; pojedyncze stuki mieszczą się w najgłośniejszych 2%.
- Na nagraniu 106 min z ~1190 wykryć w długich ciszach zostaje ~100, a z 55 oznaczonych wdechów nie ginie żaden.
- Każdy region modelu przechowuje profil `silence_db[i]`: o ile dB ciszej od poziomu mowy jest najgłośniejszy dźwięk w ±(i+1) s (zaokrąglone w górę, żeby zaokrąglenie nigdy nie ukryło wykrycia). Dzięki temu suwaki działają natychmiast, bez ponownej analizy. Wykrycia, które nie przeszłyby nawet przy 30 s i 30 dB, nie są zapisywane.
- Wariant „co najmniej 0,5 s głośnego dźwięku w otoczeniu” czyścił ciszę lepiej, ale gubił wdechy przed pierwszym zdaniem nagrania. Reguła „jakikolwiek dźwięk” jest prostsza i nie gubi wdechów.
- Rozważane i odrzucone: stały próg głośności wykrycia (cichy oddech przy słuchaniu jest równie głośny jak prawdziwe ciche wdechy), detektor mowy VAD (dźwięk z głośników też jest mową).

## Katalog aplikacji

- Model i biblioteki są częścią aplikacji, a nie danymi konta, więc leżą obok programu, a nie w katalogu domowym; aplikacja działa z dowolnego katalogu. Wersja deweloperska (`target/<profil>/oddech`) szuka ich w `<repo>/dist` o takim samym układzie, żeby nie kopiować ich do `target/` (znikałyby przy `cargo clean`).

## Dystrybucja

- Paczki: `.deb` (Debian, Ubuntu i pochodne) i AppImage (pozostałe dystrybucje). Budowane w kontenerze z Ubuntu 22.04 (glibc 2.35): paczka zbudowana na najstarszym obsługiwanym systemie działa na nowszych. Budowanie na 22.04 nie wymagało zmian w programie; dołączone biblioteki wymagają co najwyżej glibc 2.28.
- Układ jak katalog aplikacji ze źródeł: program, `lib/`, `models/` (w `.deb` w `/opt/oddech`, z dowiązaniem `/usr/bin/oddech`). Program ma w ścieżce wyszukiwania `$ORIGIN/lib`, a dołączone biblioteki FFmpeg `$ORIGIN` (ścieżka programu obejmuje tylko jego bezpośrednie zależności).
- FFmpeg dołączony, bo każda dystrybucja ma inną główną wersję (inne sonamy i ABI). Niezmieniona kompilacja LGPL z BtbN/FFmpeg-Builds, wersja 8.1 (najwyższa obsługiwana przez `ffmpeg-next` 9); tylko avcodec, avformat, avutil i swresample (~120 MB). `ffmpeg-next` bez domyślnych funkcji (tylko `codec` i `format`), więc program nie łączy się z avfilter, avdevice, swscale ani postproc. Zależność od ffmpeg-next pozwala dalej budować ze źródeł z systemowym FFmpeg.
- ONNX Runtime w paczce: biblioteka główna z wydania GPU 1.22 (bez zależności od CUDA, działa na procesorze). Dostawca CUDA z bibliotekami NVIDIA jest w osobnej paczce `oddech-cuda` (tylko `.deb`), bo zajmuje 2,9 GB; bez dostawcy program nie próbuje CUDA (patrz „CUDA i GPU”).
- `oddech-cuda` dokłada do `/opt/oddech/lib` dostawcę CUDA (ścieżka wyszukiwania `$ORIGIN/cuda`), a do `lib/cuda/` biblioteki z redystrybucyjnych archiwów NVIDIA: CUDA 12.9 (cudart, cuBLAS, cuBLASLt, cuRAND, cuFFT, NVRTC) i cuDNN 9.25 z wszystkimi podbibliotekami, które cuDNN ładuje w zależności od karty. CUDA 12.9 działa ze sterownikami od 525 (zgodność w obrębie wersji 12) i ma kod dla kart RTX 50; niepasujący sterownik lub kartę wyłapuje test GPU. Biblioteki systemowe nie są używane, więc wersja jest zawsze ta sprawdzona z ORT 1.22. `libnvrtc` dostaje ścieżkę `$ORIGIN`, bo ładuje swoje `builtins` przez `dlopen`; pozostałe biblioteki NVIDIA ją mają. Sterownik (`libcuda.so.1`) zawsze z systemu.
- Licencje NVIDIA (umowy CUDA Toolkit, Attachment A, i cuDNN) pozwalają rozpowszechniać te biblioteki razem z aplikacją, jeśli korzysta z nich tylko ona; leżą w prywatnym katalogu programu, poza ścieżkami systemowymi. Teksty umów w `licenses/NVIDIA-*.txt`.
- AppImage bez obsługi GPU: obraz jest tylko do odczytu, więc dodatek wymagałby osobnego katalogu użytkownika, a z 2,9 GB bibliotek obraz byłby kilkukrotnie większy.
- Licencje w paczce (`licenses/`): Oddech (MIT), FFmpeg (LGPL, z adresem źródeł), ONNX Runtime z informacjami o składnikach, Respiro-en oraz biblioteki Rusta wkompilowane w program (`packaging/crate_licenses.py`: nazwa, wersja, licencja i pliki licencji z graf zależności zwykłych). Wszystkie biblioteki Rusta mają licencje zezwalające (MIT, Apache-2.0, BSD, Zlib, ISC, Unlicense); `option-ext` jest na MPL-2.0, co przy niezmienionym kodzie wymaga tylko dołączenia licencji.
- Model Respiro-en w ONNX (13 MB) jest w repozytorium (`models/`), żeby budowanie paczek i wersji ze źródeł nie wymagało PyTorcha; `training/export_respiro_onnx.py` odtwarza go z wag Respiro-en.
- Wszystkie pobierane składniki mają przypięte wersje i sumy SHA-256 (`packaging/versions.sh`).
- Zależności `.deb`: biblioteki ładowane przez winit, wgpu i rfd w czasie działania (X11, xkbcommon, Vulkan, EGL, D-Bus) są wymienione jawnie, bo nie widać ich w nagłówku ELF. Okna wyboru plików używają portalu XDG, a bez niego zenity (rfd), stąd `xdg-desktop-portal | zenity` w zalecanych.
- Sprawdzone w czystych kontenerach: instalacja `.deb` z zależnościami i start z dekodowaniem nagrania na Ubuntu 22.04, 24.04, Debianie 12 i 13; AppImage na Archu.

