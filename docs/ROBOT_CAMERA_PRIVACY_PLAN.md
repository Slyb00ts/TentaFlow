# Kamery robotów — detekcja osób na żywo i anonimizacja twarzy na GPU (plan)

Status: PLAN v2 (po recenzji). Cel sprzętowy: NVIDIA DGX Spark (GB10, aarch64,
CUDA 13.0, driver 580.173, TensorRT 11.3, GStreamer 1.24.2 + plugin `nvcodec`).
Identyfikatory kodu w tym dokumencie są angielskie; opis po polsku. Odpowiedź na
niezależną recenzję wersji 1 jest w dodatku na końcu.

## Decyzje użytkownika (2026-09-22) — wiążą nad treścią poniżej

1. Anonimizacja **nieodwracalna u źródła** — brak jakiejkolwiek niezamazanej kopii (pytanie 1: tak).
2. Hosty bez NVIDIA: **jawna zaślepka z TODO** — docelowo wszystkie modele przejdą przez własny silnik inferencji działający na każdym backendzie; do tego czasu opcje prywatności na takim hoście zwracają czytelny błąd „niedostępne na tym węźle" (toggle zablokowany w UI). To świadomy, jednorazowy wyjątek od reguły CLAUDE.md nr 1 (no stubs/TODO) zatwierdzony przez użytkownika dla tego jednego miejsca; TODO musi wskazywać na przyszły silnik.
3. Domyślnie dla nowego robota: **face_blur włączony**, person_detect wyłączony.
4. Zostajemy na **CUDA EP**; TensorRT 11 ewentualnie po benchmarku.
5. Zamazywanie **całej głowy z boxa osoby** na start (jeden model YOLOv8n); SCRFD warunkowo w fazie 4.
6. Stałe opóźnienie **+1 klatki** zaakceptowane.
7. **Bez priorytetów GPU** między LLM a kamerą — w normalnej pracy nasycenie nie wystąpi; tor prywatności przy braku wyniku i tak zamazuje całą klatkę (fail-closed).

## Zmiany wprowadzone przy implementacji fazy 1 (2026-09-22) — wiążą nad §2–§4

1. **Tryb synchroniczny BEZ opóźnienia klatki.** Pomiar fazy 0 (forward + dekod ~5 ms p95) pozwala
   zrobić detekcję i blur **tej samej klatki** w sondzie, zanim klatka ją opuści. Opóźnienie +1
   klatki (decyzja 6) i wstrzymywanie bufora z §2.2 są zbędne: każda klatka jest zamazana na
   podstawie własnej detekcji. Fail-closed bez zmian: brak wyniku → cała klatka; klatka, której
   nie da się zmapować → porzucona.
2. **Źródło opcji dla kamer robotów = konfiguracja addonu** (`privacy_face_blur`,
   `privacy_person_detect` w `[config.schema]` addonów `go2` i `phone`), czytana przez core przy
   rejestracji kamery (`PrivacyOptions::for_robot_addon`). Kolumna `cameras.privacy_json` z §4.2
   odpada w fazie 1: wiersz kamery WebRTC jest kasowany przy restarcie, a każde połączenie robota
   rejestruje nowe `camera_id`, więc ustawienie w wierszu ginęłoby. Kolumna wróci w fazie 3 dla RTSP.
3. **Zmiana opcji = przebudowa pipeline'u** (stary zatrzymany w całości, nowy startuje z sondą),
   a nie przełączanie Branch B na żywo — brak okna przejściowego z niezamazanym obrazem. W fazie 1
   nowe opcje obowiązują od kolejnego połączenia robota; zastosowanie na żywo przy zapisie
   ustawień — faza 2.
4. **Gałąź do pamięci hosta zawsze wpięta, ale ≤ 5 fps** (dekymator PTS przed `cudadownload`):
   kamera robota ma stałego konsumenta (depth mapping), więc „na żądanie" sprowadzało się do „zawsze";
   5 fps zamiast pełnego fps usuwa ping-pong. Liczniki fps liczy sonda (pełna częstotliwość).
5. **Provisioning modelu `privacy-cv` zablokowany:** brak zaufanego publicznego źródła ONNX FP32
   YOLOv8n (Ultralytics publikuje tylko `.pt`; jedyny eksport ONNX na HF jest FP16). Wymaga decyzji
   (hosting własnego eksportu). Model działa po ręcznym umieszczeniu w `models/vision/`.

## Zmiana modeli (2026-09-22, decyzja licencyjna) — wiąże nad §2.4

YOLOv8n (AGPL-3.0) **wypada z projektu**: TentaFlow jest Apache-2.0 i nie chcemy, by binarka
podlegała AGPL. Faza 1 używa dwóch modeli na licencjach permisywnych, pobieranych automatycznie
z oficjalnych źródeł (weryfikacja SHA-256, adresy przypięte w `camera_cv_models.rs`,
bundle `privacy-cv`):

| Rola | Model | Licencja | Wejście | Źródło |
|---|---|---|---|---|
| osoby | **YOLOX-tiny** (COCO) | Apache-2.0 | 416², letterbox pad 114, BGR 0..255 | release Megvii na GitHub (20 MB) |
| twarze | **YuNet** (`face_detection_yunet_2023mar`) | MIT | 640², stretch, BGR 0..255 | repo OpenCV na HuggingFace (233 KB) |

Dwa detektory to dwie niezależne siatki bezpieczeństwa: twarz tuż przed obiektywem (ciało poza
kadrem) łapie YuNet, głowę odwróconą tyłem — region z boxa osoby YOLOX. Regiony twarzy są
nakładane PRZED regionami głów, bo przy nakładaniu wygrywa późniejszy, a pas głowy jest większy
i grubiej pikselizowany — twarz nie może wyjść słabiej zamazana niż otoczenie.
Próg osoby w sondzie to 0,2 (niżej niż domyślne 0,3 detektora): pominięta osoba to wyciek twarzy,
a fałszywa detekcja to tylko zamazany fragment tła.
`detector_vehicle.rs` → `detector_coco.rs` (`CocoDetector`), nowy `face_yunet.rs`; ścieżka
pojazdów (asocjacja per-truck) korzysta z tego samego modelu YOLOX.

## 0. Stan zastany (zweryfikowany w kodzie i na węźle)

| Obszar | Fakt |
|---|---|
| Źródło | Go2 dostarcza H.264 Annex-B 1280x720 @ 25 fps, IDR ~2 s, **bez audio** (`addons/go2/manifest.toml:121 audio = false`; `tentaflow-hardware/src/unitree/go2/PROTOCOL_data2_2.md`), przez `camera_register_backed_v1` (`addon/host_functions/camera.rs:373`) → `Supervisor::add_webrtc_camera` → `spawn_webrtc_session` (`session.rs:335`). |
| Graf GStreamer robota | `webrtc_source.rs:50-53`: `appsrc → tee(h264)` na **surowym Annex-B** — celowo, komentarz `webrtc_source.rs:33-45` opisuje błąd wspólnego `h264parse` przed tee (SPS w osobnym AU, Branch B budował avcC z `num_pps=0`, FATAL `No caps set`). Branch A: `decodebin(CPU) → videoconvert → RGB appsink` (mailbox, `frame_storage` = snapshot JPEG, TentaVision, depth); Branch B (na żądanie): `[SPS-gate] → h264parse → h264timestamper (webrtc_source.rs:255) → mp4mux(fMP4) → appsink` — **passthrough, bez transkodowania**. |
| Nagrania | `services/event_recorder.rs` subskrybuje ten sam hub `camera:<id>` (Branch B), trzyma pre-roll w ring bufferze (`event_recorder.rs:17-25`) i **startuje nagranie tylko przy `msg.motion.moving`** (`event_recorder.rs:1243-1250`). RTSP Branch B **już** transkoduje na CPU (`rtsp.rs:2249` `avdec_h264 → x264enc`), robot — nie. |
| NVDEC/zero-copy | Istnieje: `gst_cuda_ffi.rs` (`map_nv12_device`, `GST_MAP_CUDA`), `vision/gpu_preprocess.rs::preprocess_nv12_device_gpu` (`:901`; fused NV12→RGB→resize→normalize, **alokuje `OwnedDeviceTensor` per wywołanie i domyślnie synchronizuje urządzenie**, bo `zerocopy_map_sync=false`, `config/mod.rs:361`), ścieżka `IngestPath::NvdecNv12` + `tee_cuda` w `rtsp.rs:1530-1700`, `link_nvdec_branch` (`rtsp.rs:1900`). Używane tylko przez RTSP, a i tam `zerocopy_detect` ma default `false` (`config/mod.rs:359`) — **ścieżka zero-copy nie jest dziś ćwiczona produkcyjnie**. |
| ORT / TensorRT | `ort` 2.0.0-rc.12 `load-dynamic`; `native-libs/linux-aarch64/lib-dynamic/libonnxruntime_providers_tensorrt.so` ma `NEEDED libnvinfer.so.10` + `libnvonnxparser.so.10` (`readelf -d`), a system ma **wyłącznie `libnvinfer.so.11.3.0`** (`ldconfig -p`). Wszystkie `.runtime/models/vision/trt-cache{,-stan,-plate}` są **puste**. `ort_common.rs:712 retry_without_tensorrt` po cichu buduje sesję bez TRT → **dziś każdy model wizyjny na tym węźle działa na CUDA EP, nie na TensorRT**. |
| Modele | `detector_vehicle.rs`: YOLOv8n COCO 640 ONNX (`vision_models_dir()/yolov8n-vehicle.onnx` + `vehicle-classes.json`), filtruje id {2,5,7}; **ten sam graf ma klasę `person` (id 0)**. Plik NIE ma ścieżki provisioning (brak w `camera_cv_models.rs::BUNDLES`). SCRFD w repo to **`det_500m.onnx` z `buffalo_s`** (`scrfd.rs:4`, `vision_models.rs:156-209 ensure_scrfd_async`), przez tract na CPU, tylko dla API OpenAI-style. Dekoder (`scrfd.rs:146-160`) dobiera wyjścia po liczbie anchorów i obsługuje `(N,C)` oraz `(1,N,C)`. |
| Detekcje → UI | `detection_bus::publish_detections` → `dispatch/camera_detections.rs` → `vision-detections-overlay.js` (interpolacja po `track_id`, sync PTS przez `base_pts_ns`). Overlay wpinany tylko w `sdk-runtime/specialized-media-renderer.js::attachLiveDetections`; kafelek robota (`robots.js:1506 mountCameraTile`) go **nie ma**. Subskrypcja overlayu wywołuje `ensure_analysis` (`camera_detections.rs:217`), czyli RF-DETR; `vision_analysis.rs` publikuje w trzech miejscach (`:2101`, `:2551`, `:2785`). |
| Ustawienia addonu | `addons/go2/manifest.toml [config.schema]` używa `type = "string"` (`:99-101`); `handlers_addon_lifecycle.rs::extract_config_schema` → `AddonConfigField { id, label, field_type, description, default_value, options, required, secret }` (`message_body.rs:6406`, **bez `group`**), sortowanie po id (`:177`) → `www/js/modules/addons/settings.js`. Zapis (`addon_config_set`) **nie** powiadamia addonu; addon czyta `config_get_v1`. `tick_interval_ms = 100`. |
| Per-camera | `cameras` + `CameraPatch` (`db/repository.rs:23149`) ma już pola depth ustawiane przez host fn rejestracji — wzorzec do skopiowania. Hydratacja po restarcie: `hydrate_supervisor_from_db` (`camera.rs:265`). Ostatnie pole `WebRtcRegisterCameraInput` to `#[n(6)]` (`tentaflow-sdk-spec/src/protocol/webrtc.rs:191`). |
| Węzeł | `gst-inspect-1.0`: `nvh264dec` (`num-output-surfaces` **default 1 = „always copy"**, zmienne tylko w NULL/READY), `nvcudah264enc` z `preset/tune/zero-reorder-delay/b-frames/rate-control/bitrate/gop-size`, `cudaupload/cudadownload`, `nvjpegenc`; **brak** `cudaconvert/cudascale`. Probe: 8 równoległych sesji NVDEC→NVENC działa (~385 fps łącznie; ~3 % enkodera przy 25 fps na kamerę). Na GPU pracuje równolegle **SGLang** (`nvidia-smi`: `sglang::scheduler_TP1`) obok `tentaflow`. |
| Mojo/cubecl | Kernele Mojo w `tentaflow-infer` są wyłącznie LLM; brak kerneli obrazu. |

Wniosek: nie budujemy nowej infrastruktury GPU — łączymy istniejące klocki (NVDEC → zero-copy map →
fused preprocess → ORT → nowy kernel blur → NVENC) w jednej ścieżce i wpinamy ją w webrtc. Budżet
liczymy dla **CUDA EP** (stan faktyczny); TensorRT 11 jest opcją wymagającą przebudowy ORT (pytanie 4).

## 1. Wymagania i interpretacja

1. Detekcja osób na żywo, rysowana na kafelku robota (i wszędzie, gdzie kafelek `camera:<id>` ma overlay).
2. Anonimizacja: WSZYSTKIE twarze zamazane. Interpretacja: zamazanie jest **nieodwracalne i następuje
   przed jakimkolwiek rozgałęzieniem** — live (pełny i preview), nagrania (event recorder + pre-roll),
   snapshot JPEG (`frame_storage`), klatki dla TentaVision, depth-mapping, frame pickup/proxy przez
   mesh. Nigdzie nie istnieje niezamazana kopia. (Pytanie 1.) „Wszystkie" wyklucza predykcję jako
   gwarancję: klatka k opuszcza sondę dopiero po detekcji **na klatce k** (§2.2).
3. Zero spowolnienia: natywny fps źródła, brak ping-pongu CPU↔GPU klatek. Przez CPU przechodzą tylko
   skompresowany H.264 (wejście/wyjście), listy boxów i klatki NV12 **na żądanie** (snapshot/depth/
   TentaVision ≤ 5 fps), nigdy stały strumień. Stałe opóźnienie toru: +1 klatka (40 ms @25 fps) — pytanie 6.
4. Admin włącza/wyłącza opcje w ustawieniach addonu robota; model opcji rozszerzalny, ale **minimalny**:
   dwa przełączniki, reszta to stałe w kodzie.
5. Fail-closed: awaria/opóźnienie detektora nigdy nie pokazuje niezamazanej twarzy; jedyna reakcja na
   brak wyniku to zamazanie całej klatki.

## 2. Architektura toru GPU (NVIDIA)

### 2.1 Graf GStreamer dla kamery webrtc (nowy `webrtc_source.rs`)

```
appsrc(h264 byte-stream, au)
  → tee_h264(allow-not-linked)                              // SUROWY Annex-B, bez parse (webrtc_source.rs:33-45)
      ├─ queue → h264parse(config-interval=-1) → h264timestamper   // parse + czysty PTS TYLKO w gałęzi NVDEC
      │     → nvh264dec(cuda-device-id=0, num-output-surfaces=1)  // jawny pin: własna kopia, nie powierzchnia DPB
      │     → queue_nvdec_out(leaky=downstream, max-size-buffers=2)
      │     → [PRIVACY PROBE, src pad queue_nvdec_out]            // in-place na CUDAMemory NV12, opóźnienie 1 klatki
      │     → tee_cuda(video/x-raw(memory:CUDAMemory),NV12)
      │         ├─ queue → nvcudah264enc → h264parse → mp4mux(fMP4) → appsink    // Branch B "encoded": wpięty ZAWSZE gdy person_detect || face_blur
      │         ├─ (na żądanie) queue(leaky) → [decimator ≤5 fps] → cudadownload → capsfilter(NV12) → appsink_nv12   // mailbox: snapshot/depth/crops
      │         └─ (na żądanie) queue(leaky) → capsfilter(CUDAMemory,NV12) → appsink_detect_cuda  // TentaVision zero-copy (attach_detect_branch_cuda)
      └─ (na żądanie, TYLKO gdy person_detect=false && face_blur=false) queue → [SPS-gate] → h264parse → h264timestamper → mp4mux → appsink   // Branch B passthrough (dzisiejszy)
```

Zasady:
* **Tee na surowym Annex-B.** `h264parse` + `h264timestamper` siedzą wewnątrz gałęzi NVDEC; Branch B
  passthrough zachowuje własny parse i SPS-gate jak dziś. Wspólny parse przed tee jest udokumentowanym
  błędem (`webrtc_source.rs:33-45`) i nie wraca.
* **Blur przed tee_cuda.** Sonda modyfikuje bufor in-place zanim zobaczy go ktokolwiek. Jedyną gwarancją,
  że bufor nie aliasuje ramki referencyjnej dekodera, jest **jawny pin `num-output-surfaces=1`**
  (asercja przy budowie grafu: odczyt property po `set`). `buffer.is_writable()` nic tu nie chroni
  (bufor z `nos=1` jest zawsze „writable" w sensie refcount, a z `nos=0/4` również — przy aliasowaniu
  DPB), więc **nie ma** ścieżki `make_writable`: bufor niezapisywalny w sensie refcount (drugi ref) jest
  błędem toru i kończy sesję (`CameraIngestError::PipelineBuild`), nie cichą kopią.
* Realizacja jako **pad probe** (`gst::PadProbeType::BUFFER`), nie subclass elementu: repo używa sond
  do wszystkiego (SPS-gate, base-PTS), gstreamer-rs 0.25 bez nowych crate'ów.
* **Branch B: `encoded` (NVENC) zawsze, gdy którakolwiek opcja prywatności jest włączona;
  `passthrough` tylko gdy obie są wyłączone.** Koszt NVENC (~3 % enkodera @25 fps, potwierdzony probe)
  jest stały i akceptowany, w zamian nie ma okna przełączenia między „blur ON" a „strumień enkodowany".
  Jedyna zmiana wariantu to przejście „obie OFF" ↔ „którakolwiek ON", w kolejności: (1) sonda dostaje
  nowe opcje (od tej klatki fail-closed cała klatka do pierwszej udanej detekcji), (2) attach `encoded`,
  (3) detach `passthrough` (publisher kończy strumień, kafelek resubskrybuje —
  `close_mp4_stream_on_teardown`), (4) hub ogłasza nowy init segment, a `event_recorder` **czyści
  pre-roll** przy każdej zmianie init segmentu (nowy warunek w ring bufferze: fragmenty z poprzedniego
  wariantu nigdy nie trafiają do pliku). W przeciwnym kierunku (ON → OFF) kolejność ta sama; pre-roll
  z klatkami zamazanymi jest kasowany, więc jeden plik nagrania nigdy nie łączy obu wariantów
  (pytanie 9).
* NVENC: `nvcudah264enc preset=p3 tune=ultra-low-latency zero-reorder-delay=true b-frames=0
  rate-control=cbr bitrate=4000 gop-size=transcoder_key_int_max(fps)`, potem `h264parse
  config-interval=-1` i `mp4mux fragment-duration=100 streamable=true` (identycznie jak dziś, więc
  MSE init segment bez zmian). Strumień jest video-only (Go2 nie ma audio). PTS przechodzi przez
  enkoder; `install_base_pts_probe` na padzie sink muxa jak w RTSP, aby overlay dostał `base_pts_ns`.
* **Mailbox na żądanie.** `cudadownload` per klatka to ~35 MB/s stałego ping-pongu i łamie wymaganie 3.
  Gałąź NV12→host jest wpinana tak jak on-demand RGB w RTSP (`rtsp.rs:164-169`), z sondą-decymatorem
  (przepuszcza ≤ 5 fps), tylko gdy istnieje konsument: żądanie snapshotu (`frame_storage`), depth,
  TentaVision (ścieżka bez zero-copy), frame pickup/proxy. Bez konsumenta mailbox jest pusty, a
  `camera_frame_url` zwraca `503 retry` do pierwszej klatki (≤ 200 ms po attach).
* Dzisiejsze `decodebin(CPU)` w Branch A znika na NVIDIA. Dla hostów bez nvcodec pozostaje dzisiejszy
  graf (§8).

### 2.2 Sonda privacy (`services/camera_ingest/privacy.rs`, nowy moduł)

Gate: `#![cfg(all(any(target_os = "linux", target_os = "windows"), feature = "inference-vision-gpu",
feature = "vision-ort", feature = "vision-cuda-preprocess"))]` — ta sama brama co zero-copy.

Model opcji (jedyne dwa pola sterowane przez admina; wszystko inne to stałe w `privacy.rs`):
```rust
pub struct PrivacyOptions {            // serde JSON w cameras.privacy_json
    pub person_detect: bool,           // default false
    pub face_blur: bool,               // default false (pytanie 3)
}
const HEAD_FRACTION: f32 = 0.30;       // głowa = górne 30 % boxa osoby
const HEAD_MARGIN: f32 = 0.25;         // +25 % w każdą stronę, min. 24 px luma
const MOSAIC_BLOCK_MIN: u32 = 12;      // px luma
const RESULT_GRACE_MS: u32 = 8;        // ile sonda czeka na wynik klatki k po nadejściu k+1
```

**Dwa modele, dwa forwardy.** Faza 1 używa YOLOX-tiny (klasa `person`) i YuNet (twarze) — patrz
ta sama pula co detektor pojazdów). Region blur = **górne 30 % boxa osoby + margines** — nadmiarowo
zamazuje całą głowę zamiast precyzyjnej twarzy (pytanie 5). SCRFD (twarze) wchodzi dopiero w fazie 4
po pomiarze recall na klipie testowym: jeżeli region z boxa osoby nie pokrywa twarzy w mierzalnej
liczbie przypadków (ludzie leżący, kadry na wysokości kolan robota), dochodzi drugi forward.

**Jeden tryb: synchroniczny z opóźnieniem 1 klatki** (streaming thread gałęzi NVDEC):
1. Klatka k wchodzi do sondy. `map_nv12_device(buf, info, write=true)` (rozszerzenie `gst_cuda_ffi`;
   dziś READ-only). `preprocess_nv12_device_into(planes, &mut scratch.input, mean, std, color, stream)`
   — **nowa sygnatura** obok istniejącej: zapis do persistentnego `[1,3,640,640]` należącego do
   `PrivacyScratch`, na strumieniu sondy, bez `cudaDeviceSynchronize` (zależność decoder→kernel przez
   `cudaStreamWaitEvent` na evencie zapisanym po mapowaniu; `zerocopy_map_sync` nie dotyczy tej ścieżki).
   Forward `Session::run` (ORT, CUDA EP, IO binding na `scratch.input`) startuje **asynchronicznie**
   w wątku puli detektora; sonda trzyma własny ref do bufora k i zwraca `PadProbeReturn::Handled`
   (bufor nie idzie dalej).
2. Gdy przychodzi klatka k+1, sonda najpierw **czeka na wynik k** (do `frame_interval + RESULT_GRACE_MS`
   od wysłania). Wynik jest → dekod boxów na CPU (kilka KB), tracker (tylko `track_id`; bez predykcji),
   regiony = głowy z boxów osób klatki k ∪ regiony z klatki k-1 (**dodatkowa** konserwatywność przeciw
   migotaniu na granicy detekcji; nie zastępuje wyniku). Wyniku nie ma → **cała klatka** (fail-closed),
   licznik `privacy_fail_closed_frames`, a spóźniony wynik jest odrzucany.
3. `launch_nv12_mosaic_regions(...)` na k (na strumieniu sondy, z `cudaStreamSynchronize` przed
   unmap), unmap, `pad.push(k)` z guardem reentrancji (probe rozpoznaje własne push i przepuszcza).
   Następnie krok 1 dla k+1. EOS/flush-stop: wstrzymana klatka jest wypychana po fail-closed
   (cała klatka) albo porzucana przy flush — nigdy niezamazana.
4. Blokada streaming thread na klatkę = max(0, forward − frame_interval) + blur (< 0.5 ms). Przy
   forward p95 ≤ 40 ms dropów nie ma; powyżej — `queue_nvdec_out` (leaky) gubi klatki w całości, a
   sonda liczy `privacy_dropped_frames`. Nie ma trybu Async ani `detect_every_n`: obie rzeczy
   przepuszczają co najmniej jedną klatkę nowej twarzy.
5. Publikacja: `detection_bus::publish_detections(camera_id, ts, pts_ns(k), proc_ms, false, persons,
   MotionSignal::none())` (klasa `person`, `track_id`). Twarze/głowy nie są publikowane. Logi na tej
   ścieżce niosą **wyłącznie liczniki, czasy i id** (boxy to dane osobowe — jak transkrypty w Meeting Bot).
   Metryki `stage_metrics`: `privacy_forward_ms`, `privacy_wait_ms`, `privacy_blur_ms`,
   `privacy_fail_closed_frames`, `privacy_dropped_frames`.
6. Reset stanu (tracker, regiony k-1, licznik klatek, wstrzymana klatka): `SessionCommand::Restart`,
   ponowne połączenie WebRTC (nowy `appsrc` pump), zmiana opcji.

**Jeden publikator.** Gdy kamera ma `person_detect || face_blur`, sonda jest jedynym źródłem detekcji:
`camera_detections.rs:217` sprawdza `privacy::is_privacy_camera(&camera_id)` i **nie woła
`ensure_analysis`** (RF-DETR nie ma na kamerze robota nic do roboty). `vision_analysis.rs` pozostaje
bez zmian; nie ma mechanizmu `offer_enrichment`.

**Kolejkowanie wielu kamer.** Jedna pula sesji ORT (`SessionPool`, jak `detector_vehicle`) o
rozmiarze `min(privacy_cameras, 2)` na wspólnym strumieniu per sesja; kolejka FIFO, a każda kamera
ma własny deadline (krok 2). Przekroczenie deadline'u = fail-closed tej klatki, nigdy drop innej kamery.
Priorytety strumieni CUDA działają tylko wewnątrz procesu; wobec SGLang w osobnym procesie nie ma
mechanizmu pierwszeństwa — benchmark §10 mierzy z aktywnym LLM, decyzja w pytaniu 7.

### 2.3 Kernel blur (`cuda/nv12_mosaic_regions.cu`)

* Wejście: wskaźniki Y/UV (device), stride'y, W/H, tablica prostokątów (device, do 64), rozmiar
  bloku `b = clamp(max(w_rect, h_rect)/8, MOSAIC_BLOCK_MIN, 48)` (px luma).
* Pass 1: dla każdego (rect, blok) redukcja średniej Y oraz U/V (UV na siatce /2, współrzędne
  parzyste) do bufora `means`. Pass 2: zapis średniej do wszystkich pikseli bloku. Wynik jest
  nieodwracalny (informacja zredukowana do 1/b² na kanał). Bez wariantu gaussowskiego (stała estetyka).
* Tryb `whole_frame`: ten sam kernel z jednym rectem (0,0,W,H) i `b=32`.
* Kompilacja: dopisać do listy w `build.rs::compile_cuda_preprocess` (`libtf_nv12_mosaic.a`).
  Test parity: host oracle w Rust (ta sama definicja bloków), porównanie bit-exact na syntetycznym NV12.
* Koszt 720p: < 0.2 ms; cała klatka < 0.4 ms.

### 2.4 Modele, licencje, provisioning

| Rola | Model | Licencja | Ścieżka | Uwagi |
|---|---|---|---|---|
| Osoby (faza 1) | **YOLOX-tiny** COCO 416 (`yolox_tiny.onnx`) | **Apache-2.0** (Megvii) | `detector_coco.rs`: dekod siatek stride 8/16/32, letterbox, BGR 0..255; ta sama pula sesji co asocjacja pojazdów | Jeden forward daje osoby i pojazdy. |
| Twarze (faza 1) | **YuNet** `face_detection_yunet_2023mar.onnx` | **MIT** (OpenCV Zoo) | `face_yunet.rs`: 3 stride'y, score = √(cls·obj), stretch 640, BGR 0..255 | Druga, niezależna warstwa wykrywania. |
| (odrzucone) | YOLOv8 / YOLOv8-face | AGPL-3.0 | — | licencja niezgodna z Apache-2.0 projektu (decyzja z 2026-09-22). |

Provisioning: `yolov8n-vehicle.onnx` dopisać jako plik do istniejącego bundle `rfdetr-adr` (tam gdzie
jest już używany przez detektor pojazdów) **i** do nowego `CvBundle { engine_id: "privacy-cv",
files: [yolov8n-vehicle.onnx, vehicle-classes.json(embedded)] }` w `camera_cv_models.rs::BUNDLES`
(pliki wspólne między bundle'ami są dozwolone: `effective_files()` liczy po nazwie w
`vision_models_dir()`), plus manifest katalogu `tentaflow-containers/vision/_services/privacy-cv.toml`
(`runtime = "embedded"`, `feature_flag = "vision-cuda-preprocess"`, `platforms = ["linux","windows"]`).
Deploy = pobranie plików + **warm-up sesji ORT przy deployu, nie przy pierwszej klatce**. Na CUDA EP
warm-up to sekundy (cuDNN autotune); jeżeli ORT zostanie przebudowany pod TensorRT 11 (pytanie 4),
w tym samym kroku buduje się silnik do `trt-cache-privacy/` (dziesiątki sekund–minuty). Wymaga
hostowania pliku pod release URL (pytanie 10).

### 2.5 Budżet opóźnienia (720p25, GB10, **CUDA EP**) — zmierzony w fazie 0 (2026-09-22)

| Etap | ms | Źródło |
|---|---|---|
| NVDEC + zapis in-place + NVENC (cały tor bez detekcji) | — | **zmierzone**: 450–650 fps @720p (1,5–2,2 ms/klatkę łącznie), więc 25 fps ma ~18× zapasu |
| zapis w pamięci GPU (sonda, `cudaMemset2D` 256×256 + sync) | **0,1** | zmierzone; jednorazowo ~200 ms inicjalizacji kontekstu CUDA przy pierwszej klatce (przenieść do startu sesji) |
| YOLOX-tiny 416 + YuNet 640, ORT **CUDA EP** | **p50 3,2 + 1,6 / p95 3,6 + 2,1** | zmierzone na klatkach NV12 przez preprocess GPU (pool=2) |
| preprocess kernel | 0,15 | szacunek (istniejący kernel) |
| dekod boxów + tracker | 0,2 | szacunek |
| mosaic kernel | 0,3 | szacunek (zapis 256² zmierzony: 0,1) |
| opóźnienie strukturalne | 40 | 1 klatka — klatka k wychodzi po nadejściu k+1 (decyzja 6) |
| mp4mux + hub | 0,5 | szacunek |
| **Razem dodane (glass-to-glass)** | **~45 ms** | z czego ~5 ms pracy; GPU-busy sondy ~12 % okna klatki |

**Wnioski fazy 0:**
- **TensorRT niepotrzebny** (decyzja 4 potwierdzona pomiarem): CUDA EP daje 3,1 ms p95, czyli ~8 % budżetu klatki 40 ms. Przebudowa ORT pod TRT 11 spada z planu, dopóki pomiar pod obciążeniem nie pokaże inaczej.
- **Zapis in-place na buforze `nvh264dec` jest bezpieczny** przy `num-output-surfaces=1`: 300 klatek, GOP 50, ruch przechodzący przez modyfikowany obszar — 0 klatek z różnicą poza obszarem względem przebiegu bez zapisu; kontrola deterministyczności (dwa przebiegi bez zapisu) — 0 różnic. Ten sam wynik przy `nos=0` w tym teście, ale plan i tak przypina `nos=1` (jedyna udokumentowana gwarancja kopii). Uwaga metodyczna: źródłem testu musi być stały plik H.264 — `x264enc` na żywo jest niedeterministyczny i daje fałszywe różnice.
- **Otwarte:** pomiar przy obciążonym LLM. SGLang na tym węźle to rank 1 z `--tp-size 2` (head `10.10.10.24:5026`), w czasie pomiaru API nie odpowiadało, a GPU było bezczynne (0 %). Pomiar powtórzyć w fazie 1 przy działającym LLM; decyzja 7 (bez priorytetów) pozostaje.
- Narzędzia fazy 0 (poza repo, do odtworzenia): sonda C na `identity` po `nvh264dec` z mapowaniem `GST_MAP_CUDA|WRITE`, benchmark ORT w Rust na `native-libs/linux-aarch64/lib-dynamic/libonnxruntime.so.1.24.0` z `ep::CUDA`.

## 3. Zachowanie fail-closed

| Sytuacja | Zachowanie (face_blur=ON) |
|---|---|
| Przed pierwszą udaną detekcją (ładowanie modeli, warm-up) | cała klatka mosaic; wideo płynie; badge na kafelku „anonimizacja: inicjalizacja" (health `status_message`) |
| Wynik dla klatki k nie nadszedł do `frame_interval + RESULT_GRACE_MS` | cała klatka; spóźniony wynik odrzucony |
| Błąd inference (sesja padła) | cała klatka do czasu odbudowy puli; health `degraded` |
| Sonda nie może zbudować toru (brak nvcodec/ORT-CUDA) | Branch B **nie** jest wpinany, mailbox nie dostaje klatek: „prywatność niedostępna na tym węźle" — wideo OFF zamiast niezamazane (pytanie 2) |
| Bufor z drugim refem (naruszenie kontraktu `nos=1`) | błąd sesji (`PipelineBuild`), nigdy pominięcie blura ani kopia |
| Przepełnienie (leaky queue) | klatka wypada w całości; nigdy nie przechodzi niezamazana |
| Restart sesji / reconnect WebRTC / zmiana opcji | reset trackera i regionów k-1; pierwsza klatka nowej sesji = cała klatka do pierwszej detekcji |

Nie istnieje wariant „podtrzymaj ostatnie boxy": jest fail-open dla osoby, która właśnie weszła w kadr.

## 4. Ustawienia: model, przechowywanie, zastosowanie na żywo

### 4.1 Manifest addonu (rozszerzalny)
`addons/go2/manifest.toml [config.schema]`:
```toml
privacy_person_detect = { type = "bool", label = "Detekcja osób na żywo", default = "false", group = "Prywatność i detekcja" }
privacy_face_blur     = { type = "bool", label = "Anonimizacja twarzy (zamazanie głów wszystkich osób)", default = "false", group = "Prywatność i detekcja" }
```
* `extract_config_schema` zna `bool` (obok `string`, którego używają istniejące klucze go2).
* Nowe pole schematu `group` → `AddonConfigField.group: String` z `#[serde(default)]`
  (`message_body.rs:6406`; pole append-only) → `settings.js` renderuje nagłówki sekcji; sortowanie
  w `handlers_addon_lifecycle.rs:177` po `(group, id)`.
* Nowe opcje w przyszłości = nowe klucze `privacy_*` (patrz konwencja w 4.2); `PrivacyOptions::from_config`
  ignoruje nieznane i przyjmuje defaulty.

### 4.2 Źródło prawdy dla toru = kamera; zastosowanie bez ankietowania
* Migracja: `ALTER TABLE cameras ADD COLUMN privacy_json TEXT NOT NULL DEFAULT '{}'`; `CameraRow`,
  `CameraPatch.privacy: Option<PrivacyOptions>`, `CameraConfig.privacy`.
* **Core stosuje `privacy_*` bezpośrednio przy zapisie konfiguracji addonu.** Klucze o prefiksie
  `privacy_` w konfiguracji addonu, który zarejestrował kamery przez `camera_register_backed_v1`, są
  konwencją core (udokumentowaną w `addon-sdk`): handler `AddonConfigSetRequest` po zapisie woła
  `privacy::apply_addon_config(addon_id, values)` → `update_camera(CameraPatch)` dla każdej kamery
  tego addonu + `SessionCommand::UpdatePrivacy(PrivacyOptions)` → sonda podmienia
  `ArcSwap<PrivacyOptions>`, sesja przepina Branch B wg §2.1. Odpowiedź handlera niesie
  `applied_cameras: Vec<String>` — UI ma sygnał „zastosowano", zamiast okna 3–4 s bez potwierdzenia.
  Addon nie ankietuje `config_get` w `on_tick` i nie potrzebuje nowego pola w
  `WebRtcRegisterCameraInput`: przy rejestracji core czyta zapisaną konfigurację addonu
  (`addon_config` dla `addon_id`) i ustawia `privacy` w `CameraConfig` sam. Kamery RTSP dostaną te
  same pola przez panel kamer (faza 3) — ta sama `CameraPatch`.
* Hydratacja po restarcie: `hydrate_supervisor_from_db` (`camera.rs:265`) przekazuje `privacy_json`
  do `CameraConfig`.
* Bez restartu procesu ani pipeline'u; zmiana wariantu Branch B daje ~1 s przerwy na kafelku tylko przy
  przejściu „obie OFF" ↔ „którakolwiek ON".

### 4.3 Status modeli w UI
* `AddonConfigGetResponse.requirements: Vec<AddonRequirement { id, label, status: installed|missing|installing|unsupported_host, engine_id }>`
  (`#[serde(default)]`) z nowej sekcji manifestu `[[requires.vision_engine]] engine = "privacy-cv"`
  (generyczne: każdy addon może zadeklarować silnik CV; ten sam mechanizm obsłuży `depth-native`).
* `settings.js`: sekcja „Wymagane modele" — `tf-chip` statusu + `tf-button` „Zainstaluj" →
  istniejący `ServiceDeployRequest` (engine `privacy-cv`) z postępem jak przy deployu serwisu; przy
  `unsupported_host` oba toggle są disabled z opisem.
* Ten sam blok w oknie instalacji addonu (checkbox „zainstaluj od razu", pytanie 11).
* i18n: klucze `addon_settings.requirements.*`, `robots.camera.privacy.*` w **5 lokalizacjach**
  (pl/en/de/fr/es); `robots.js` dziś nie używa `I18n` — nowe teksty przez `I18n.t`.

## 5. Detekcje na kafelku robota
* Wyciągnąć `attachLiveDetections` z `sdk-runtime/specialized-media-renderer.js` do
  `modules/vision-detections-overlay.js::attachOverlayToVideoStreamTile(tile, cameraId, registerCleanup)`
  i użyć w obu miejscach (`robots.js::mountCameraTile` + renderer). Zero duplikacji.
* `KLASA_KOLORY['person']`, etykieta i18n `detections.class.person`. Głowy/twarze nie są rysowane.
* Overlay synchronizuje po PTS: `pts_ns` detekcji = PTS bufora CUDA klatki k (ta sama oś co
  `base_pts_ns` z sondy na sink muxa Branch B encoded); opóźnienie 1 klatki jest już w strumieniu,
  więc overlay nie potrzebuje korekty.
* Badge stanu prywatności na kafelku: „twarze: anonimizacja aktywna / inicjalizacja / niedostępna"
  z `CameraHealth.status_message` (już przesyłany).

## 6. Nagrania i retencja
* Event recorder i pre-roll konsumują Branch B → przy którejkolwiek opcji ON strumień jest enkodowany
  po sondzie, więc nagrania są zamazane u źródła; retencja/klasy bez zmian. Nie istnieje kopia
  niezamazana (pytanie 1).
* Pre-roll jest **czyszczony przy zmianie init segmentu** (§2.1), więc plik nigdy nie miesza wariantów.
* Snapshot JPEG (`frame_storage`, `camera_frame_url`), depth-mapping, frame pickup/proxy przez mesh
  i relay huba `camera:<id>` czytają wyłącznie dane po `tee_cuda` → zamazane. Test (b) w §10 sprawdza
  to na snapshotcie i frame-pickup, nie tylko na fMP4.
* Rekorder startuje tylko przy `msg.motion.moving` (`event_recorder.rs:1243`), którego sonda nie liczy
  (`MotionSignal::none()`). Domyślnie kamera robota **nie nagrywa** — publikacja osób nie uruchamia
  plików; jeżeli nagrania robota mają istnieć, potrzebny jest sygnał ruchu z różnicy boxów osób
  (pytanie 12).
* Bitrate nagrania = bitrate NVENC (4 Mb/s CBR 720p) zamiast bitrate'u robota; stała w `privacy.rs`
  (`ENCODE_BITRATE_KBPS`), nie ustawienie.

## 7. Reużycie dla RTSP
* Ta sama sonda wpinana w `link_nvdec_branch` (po `queue_nvdec_out`, przed `tee_cuda`/`tee_decode`)
  gdy `CameraConfig.privacy.face_blur || person_detect`; wymaga ścieżki `NvdecNv12` (dziś za
  `zerocopy_detect=false`, więc faza 3 zaczyna od włączenia i przećwiczenia tej ścieżki na RTSP).
  Branch B RTSP `attach_mp4_branch_preview` (CPU `avdec+x264enc`) dostaje wariant
  `tee_cuda → nvcudah264enc`: bez `cudascale` w 1.24 preview 720p z 4K wymaga skalowania w naszym
  kernelu (dodatkowy wariant preprocess → NV12 720p) — faza 3, osobna decyzja.
* Panel kamer: pola `privacy` w `camera_update` + UI (tf-toggle) — te same `PrivacyOptions`.

## 8. Co gdzie działa (jawnie)

| Host | Dekod | Detekcja | Blur | Enkod | Status |
|---|---|---|---|---|---|
| NVIDIA Linux/Windows (`gpu-cuda`/`vision-cuda`) | NVDEC (nvcodec) | ORT CUDA EP FP16 (TRT po przebudowie ORT) | kernel CUDA in-place | NVENC | pełna funkcja, natywny fps, +1 klatka |
| AMD/Intel (`gpu-vulkan`, wgpu/Burn) | VA-API możliwy, brak mapowania VA→wgpu w repo | Burn/wgpu (YOLO wymagałby portu) | brak | VA-API | **niedostępne** w tym planie; UI: `unsupported_host` — pytanie 2 |
| Apple Metal | VideoToolbox | MLX/Metal | Metal compute | VideoToolbox | poza zakresem (faza 5, projekt osobny) |
| Vision-worker (Stage B) | kamery robotów są core-owned → nie dotyczy | | | | |

Nie proponujemy cichego CPU-fallbacku (`avdec → CPU blur → x264enc`): łamałby wymaganie „wszystko
na GPU" i regułę CLAUDE.md „no fallbacks"; działanie na AMD/Intel to osobna faza (pytanie 2).

## 9. Zmiany w kodzie (lista plików)

Core Rust (`tentaflow-core`):
* `cuda/nv12_mosaic_regions.cu` (nowy) + `build.rs::compile_cuda_preprocess` (lista źródeł).
* `src/vision/gpu_preprocess.rs`: FFI `launch_nv12_mosaic_regions`, `preprocess_nv12_device_into`
  (persistentny tensor, strumień + event zamiast device sync), `PrivacyScratch`.
* `src/services/camera_ingest/gst_cuda_ffi.rs`: `map_nv12_device(buffer, info, write: bool)`.
* `src/services/camera_ingest/privacy.rs` (nowy): `PrivacyOptions`, stałe, `PrivacyState`,
  `install_privacy_probe` (opóźnienie 1 klatki, guard reentrancji, EOS/flush), fail-closed, publikacja,
  `is_privacy_camera`, `apply_addon_config`, pula sesji z deadline'ami.
* `src/services/camera_ingest/webrtc_source.rs`: graf z §2.1 (parse w gałęzi NVDEC, `tee_cuda`, Branch B
  encoded/passthrough, mailbox on-demand z decymatorem), `attach_mp4_branch_webrtc(variant)`.
* `src/services/camera_ingest/session.rs`: `SessionCommand::UpdatePrivacy`, kolejność przepinania
  Branch B, reset sondy przy `Restart`/reconnect, `CameraConfig.privacy`.
* `src/services/event_recorder.rs`: czyszczenie pre-rollu przy zmianie init segmentu.
* `src/services/camera_ingest/rtsp.rs`: sonda w `link_nvdec_branch` (faza 3).
* `src/dispatch/camera_detections.rs:217`: pominięcie `ensure_analysis` dla kamer prywatności.
* `src/services/camera_ingest/tracker.rs`: bez zmian (używany tylko do `track_id`).
* `src/vision/detector_vehicle.rs`: `decode_yolo_batch` z zestawem klas (`person`, pojazdy), nazwa
  pliku bez zmian; `src/vision/runners.rs`: `get_person_detector` (ta sama pula).
* `src/vision/camera_cv_models.rs`: `yolov8n-vehicle.onnx` w bundle `rfdetr-adr` i nowym `privacy-cv`;
  `tentaflow-containers/vision/_services/privacy-cv.toml`.
* `src/db/migrations.rs`, `src/db/repository.rs` (`cameras.privacy_json`, `CameraRow`, `CameraPatch`, `update_camera`).
* `src/addon/host_functions/camera.rs`: `camera_register_backed_v1` czyta konfigurację addonu i ustawia
  `privacy`; `hydrate_supervisor_from_db` przekazuje `privacy_json`.
* `src/api/dashboard/handlers_addon_lifecycle.rs`: `group` w schemacie, sortowanie `(group, id)`,
  `requirements` w `AddonConfigGetResponse`, `apply_addon_config` + `applied_cameras` w odpowiedzi
  `AddonConfigSet`; `tentaflow-protocol` (nowe pola z `#[serde(default)]`, append-only).
* `tentaflow-sdk-spec`: bez zmian w `WebRtcRegisterCameraInput`/`CameraUpdateInput` w fazach 1–2
  (privacy stosuje core); `CameraUpdateInput.privacy` (`#[n(<następny>)]`) dopiero z panelem kamer w fazie 3.

Addon go2 (`manifest.toml`): klucze `privacy_*` z `group`, `[[requires.vision_engine]]`. Brak zmian
w `lib.rs`.

Dashboard (`www/js`): `modules/vision-detections-overlay.js` (helper + `person`),
`sdk-runtime/specialized-media-renderer.js` (użycie helpera), `modules/robots.js` (overlay + badge + I18n),
`modules/addons/settings.js` (grupy, wymagania, install, „zastosowano dla N kamer"),
`www/i18n/{pl,en,de,fr,es}.json`.

## 10. Testy i benchmark

Jednostkowe (bez GPU):
* `privacy::regions_for_frame`: głowa z boxa, margines, klamry, parzystość UV, unia z k-1, maszyna
  stanów opóźnienia 1 klatki i fail-closed (zegar wstrzykiwany; EOS z wstrzymaną klatką; spóźniony wynik).
* `PrivacyOptions::from_config` (defaulty, nieznane klucze) i `apply_addon_config` (tylko kamery addonu).
* `decode_yolo_batch` z zestawem klas (person + vehicle) — parity z dotychczasowym filtrem pojazdów.
* Pre-roll: zmiana init segmentu czyści ring buffer.
* Kernel mosaic: host oracle vs GPU (bit-exact) — `#[ignore]` bez CUDA, jak `zerocopy_verify`.

Integracyjne (GPU, `TF_GPU_TESTS=1`, wzór `tests/camera_rtsp_integration.rs` + `fakefile.rs`):
* Klip 720p25 z osobami (pytanie 13) przez pełny graf; asercje:
  (a) fps wyjścia == fps wejścia, 0 dropów przez 30 s (`FrameCounters`), opóźnienie == 1 klatka;
  (b) detektor twarzy (SCRFD, tract) uruchomiony na WYJŚCIU — zdekodowane fMP4 Branch B, snapshot JPEG
  z `frame_storage` **i** bajty z frame pickup — wykrywa ≤ 2 % twarzy z wejścia;
  (c) `person` publikowane z `track_id` i `pts_ns`; (d) `AddonConfigSet` na żywo przepina Branch B,
  kafelek dostaje nowy init segment, pre-roll rekordera jest pusty po przełączeniu;
  (e) brak pliku modelu → 100 % klatek `fail_closed`; (f) `Restart` sesji → pierwsza klatka `fail_closed`.

Benchmark (`benches/camera_privacy_fps.rs`, criterion + raport tekstowy; uruchamiany na Spark
**z aktywnym SGLang generującym tokeny** i osobno na pustym GPU):
* Źródła: 720p25, 1080p30, 2 równoległe kamery 720p25. Metryki: delivered fps vs source fps,
  p50/p95 `privacy_forward_ms`/`privacy_wait_ms`/`privacy_blur_ms`, udział `fail_closed`,
  p95 glass-to-glass (PTS wejścia → chunk fMP4), GPU util, CPU % procesu.
* Akceptacja (CUDA EP): fps = źródło (0 dropów w 60 s), forward p95 < 25 ms @720p przy 1 kamerze i
  < 38 ms przy 2 kamerach z aktywnym LLM, `fail_closed` < 0,5 % klatek poza inicjalizacją,
  CPU < 15 % rdzenia/kamerę. Niespełnienie przy 2 kamerach = decyzja o przebudowie ORT pod TRT 11
  (pytanie 4), nie zmiana projektu.

## 11. Fazy dostarczania i kryteria akceptacji

| Faza | Zakres | Akceptacja |
|---|---|---|
| 0 Spike (1–2 dni) | webrtc: NVDEC → tee_cuda → NVENC Branch B (bez privacy), parse w gałęzi NVDEC, pin `nos=1` + zapis in-place na klipie z długim GOP; pomiar YOLOv8n-640 na CUDA EP z aktywnym SGLang; decyzja TRT 11 (pytanie 4) | kafelek robota gra z NVENC; forward p95 zmierzony i wpisany do §2.5 |
| 1 Tor GPU | §2.2–2.4: sonda z opóźnieniem 1 klatki, kernel, detektor osób ORT, fail-closed, publikacja, bundle `privacy-cv`, mailbox on-demand, pominięcie `ensure_analysis` | testy jednostkowe + integracyjne (a)(b)(c)(e)(f); bench §10 |
| 2 Ustawienia i UI | §4, §5: manifest/grupy, `privacy_json`, `apply_addon_config`, przepinanie Branch B + flush pre-rollu, overlay w robots.js, wymagania modeli + install, i18n ×5 | admin włącza/wyłącza bez restartu z potwierdzeniem; status modeli poprawny; test (d) |
| 3 RTSP + nagrania | §6, §7: `zerocopy_detect` na RTSP, sonda w `link_nvdec_branch`, panel kamer, weryfikacja nagrań/pre-roll/snapshot | nagranie i snapshot zamazane; RTSP Branch B bez x264 na NVIDIA |
| 4 Hardening | pomiar recall regionu głowy vs twarz; SCRFD `det_500m` przez ORT tylko gdy pomiar tego wymaga; telemetria w Analytics; `docs/BENCH_ROBOT_PRIVACY_SPARK.md`; ewentualna przebudowa ORT pod TRT 11 | 24 h soak bez dropów z aktywnym LLM |
| 5 (opcja) | AMD/Intel VA-API + Burn/wgpu, Apple Metal | osobny plan |

## 12. Ryzyka i mitigacje
1. TensorRT EP niezaładowalny (ABI `libnvinfer.so.10` vs system 11.3) → plan liczy na CUDA EP;
   przebudowa ORT pod TRT 11 to osobna decyzja z własnym pomiarem (pytanie 4). Niezależnie od tego
   `retry_without_tensorrt` powinien logować na `warn` raz na proces, że TRT nie działa — dziś maskuje
   problem na wszystkich modelach.
2. Zapis in-place na powierzchni DPB → jawny pin `num-output-surfaces=1` + asercja property; test
   z klipem długiego GOP (artefakty referencyjne byłyby widoczne natychmiast).
3. Wspólny kontekst CUDA nvcodec/ORT/kernele → jak dla zero-copy (device 0 primary ctx); test
   `cudaPointerGetAttributes` zostaje; ścieżka zero-copy jest dziś nieużywana produkcyjnie, więc
   faza 0 ją najpierw ćwiczy.
4. Ciągłość PTS/DTS dla MSE po enkoderze (jitter WiFi robota) → `h264timestamper` w gałęzi NVDEC,
   `zero-reorder-delay`, brak B-ramek; test resubscribe po zaniku RTP.
5. Region głowy z boxa osoby nie pokrywa twarzy (osoba leżąca, kadr od dołu) → pomiar recall w fazie 4;
   SCRFD jako drugi forward tylko wtedy.
6. Licencja modeli → rozstrzygnięte: YOLOX-tiny (Apache-2.0) + YuNet (MIT), YOLOv8 usunięty.
7. Kontencja GPU z SGLang w osobnym procesie → brak mechanizmu priorytetu między procesami; benchmark
   z aktywnym LLM; przy przekroczeniu budżetu tor traci jakość (fail-closed), nie prywatność (pytanie 7).
8. Limity sesji NVENC/NVDEC na GB10 → 8 równoległych par potwierdzone w probe.
9. Brak `cudaconvert/cudascale` w GStreamer 1.24 → własne kernele; preview 720p z 4K dla RTSP wymaga
   własnego skalowania (faza 3).
10. Okno przełączenia Branch B → jedyna zmiana wariantu przy „obie OFF ↔ którakolwiek ON", kolejność
    encoded-first + flush pre-rollu (§2.1).

## Pytania do użytkownika

Blokujące (bez odpowiedzi nie zaczynamy fazy 0/1):
1. [BLOKUJĄCE] Czy anonimizacja ma być **nieodwracalna u źródła** (brak jakiejkolwiek niezamazanej
   kopii: live, nagrania, snapshoty, klatki dla TentaVision/depth, frame pickup)? Alternatywa: oryginał
   w osobnym, szyfrowanym nagraniu z dostępem audytowanym — inny (większy) projekt.
2. [BLOKUJĄCE] Host bez NVIDIA (AMD/Intel/Apple): czy „blur ON, tor GPU niedostępny" ma oznaczać
   **brak wideo** (fail-closed), czy blokadę włączenia opcji w UI z komunikatem? Czy planować fazę 5?
3. [BLOKUJĄCE] Wartości domyślne dla nowego robota: `face_blur` włączone czy wyłączone? `person_detect`?
4. [BLOKUJĄCE] TensorRT: czy przebudować ORT (`native-libs`) pod TensorRT 11 przed fazą 0 (dłuższa
   praca w `scripts/native-libs`, dotyczy też RF-DETR/OCR, które dziś po cichu działają na CUDA EP),
   czy zostać na CUDA EP i decydować po benchmarku §10?
5. [BLOKUJĄCE] Czy dopuszczalne jest zamazanie **całej głowy z boxa osoby** (górne 30 % + margines —
   nadmiarowe, jeden model) zamiast precyzyjnej twarzy? SCRFD dochodziłby dopiero po pomiarze recall.
6. [BLOKUJĄCE] Czy akceptowalne jest stałe opóźnienie +1 klatki (40 ms @25 fps) w torze, konieczne, by
   klatka wychodziła dopiero po detekcji na niej samej?
7. [BLOKUJĄCE] Priorytet przy nasyconym GPU: LLM (SGLang) czy kamera? Między procesami nie ma
   priorytetów CUDA; jeśli kamera ma pierwszeństwo, LLM musi dostać limit (np. `--mem-fraction`/
   mniejszy batch), jeśli LLM — tor prywatności degraduje do zamazywania całej klatki.

Pozostałe:
8. Licencja: **rozstrzygnięte** — YOLOX-tiny (Apache-2.0) + YuNet (MIT) zamiast YOLOv8n (AGPL).
   Apache-2.0 (YOLOX-nano / RT-DETR) kosztem dodatkowej integracji?
9. Po wyłączeniu `face_blur` pre-roll zawierający zamazane fragmenty jest kasowany (plik nigdy nie
   miesza wariantów). Czy to właściwe, czy pre-roll ma przetrwać przełączenie?
10. Gdzie hostować plik bundle `privacy-cv` (release URL), czy dopuszczalne pobieranie bezpośrednio
    z upstreamu (jak `ensure_scrfd_async` z GitHub InsightFace)?
11. Instalacja modeli: automatycznie przy instalacji addonu Go2 (pobranie ~6 MB + warm-up), czy tylko
    przycisk „Zainstaluj" w ustawieniach?
12. Czy nagrania kamery robota mają w ogóle istnieć? Rekorder wymaga dziś sygnału ruchu w strefie
    (`motion.moving`), którego sonda nie liczy; opcje: brak nagrań (domyślnie w planie), sygnał ruchu
    z różnicy boxów osób, albo nagrywanie ręczne.
13. Czy dysponujecie klipem testowym 720p z osobami/twarzami (zgody RODO) do testów integracyjnych i
    pomiaru recall, czy mamy użyć klipu syntetycznego/otwartego?
14. Czy na overlayu pokazywać liczbę „osób z zamazaną głową: N" (bez boxów głów), czy tylko ramki osób?

## Dodatek: Odpowiedź na recenzję (v1 → v2)

Każdy punkt zweryfikowano na węźle (`readelf -d`, `ldconfig -p`, `gst-inspect-1.0`, `nvidia-smi`,
listing `trt-cache*`) i w kodzie przed zmianą planu.

| Punkt | Werdykt | Co zmieniono / dlaczego nie |
|---|---|---|
| K1 TRT EP nie ładuje się | **Słusznie.** `NEEDED libnvinfer.so.10`, system ma tylko 11.3.0, cache puste, `retry_without_tensorrt` maskuje. | Budżet §2.5 przeliczony na CUDA EP; TRT 11 = pytanie 4; ryzyko 1 + postulat `warn` w retry. |
| K2 parse przed tee | **Słusznie** (`webrtc_source.rs:33-45`, timestamper `:255`). | Tee na surowym Annex-B; parse+timestamper w gałęzi NVDEC. |
| K3 `nos` default 1, `is_writable`/`make_writable` martwe | **Słusznie** (`gst-inspect`: default 1 = always copy). | Jawny pin + asercja; usunięto obie ścieżki; drugi ref = błąd sesji. |
| K4 SCRFD to `det_500m`, nie `det_2.5g` | **Słusznie co do faktu**; teza „dekoder nie do reużycia" **nie** — `scrfd.rs:146-160` dobiera wyjścia po liczbie anchorów i obsługuje `(N,C)`/`(1,N,C)`, więc 2.5G-KPS też by przeszedł. | Zostajemy przy `det_500m` (już pobierany), SCRFD i tak przesunięty do fazy 4. |
| K5 `preprocess_nv12_device_gpu` alokuje i sync-uje | **Słusznie.** | Nowa sygnatura `preprocess_nv12_device_into` (tensor persistentny, event zamiast device sync). |
| K6 trzy miejsca publikacji | **Słusznie**, ale nieistotne po zmianie: `vision_analysis.rs` nie jest dotykany. | `ensure_analysis` pominięte dla kamer prywatności; `offer_enrichment` usunięty. |
| K7 `n(7)`, `hydrate_supervisor_from_db`, `type = "string"`, brak `group`, sort `:177` | **Słusznie.** | Fakty poprawione; `group` z `#[serde(default)]`; pole `WebRtcRegisterCameraInput.privacy` w ogóle wypadło (privacy stosuje core). |
| Async / `detect_every_n` przepuszczają klatkę | **Słusznie.** | Jeden tryb z opóźnieniem 1 klatki; predykcja i `detect_every_n` usunięte. |
| `HoldLastBoxes` fail-open | **Słusznie** jako reakcja na brak wyniku. Unia z regionami k-1 przy **udanej** detekcji k jest zachowana — jest tylko dodatkowa (więcej blura), nie zastępuje wyniku. | Fail-closed = wyłącznie cała klatka. |
| Okno przełączenia Branch B i pre-roll | **Słusznie.** | Encoded zawsze przy którejkolwiek opcji ON; kolejność encoded-first; flush pre-rollu przy zmianie init segmentu. |
| Mailbox `cudadownload` per klatka | **Słusznie.** | Gałąź on-demand z decymatorem ≤ 5 fps. |
| RF-DETR nadal przez `ensure_analysis` | **Słusznie.** | Pominięcie w `camera_detections.rs:217`. |
| Propagacja ustawień 3–4 s bez potwierdzenia | **Słusznie.** | Core stosuje `privacy_*` przy `AddonConfigSet`, odpowiedź z `applied_cameras`; addon bez pollingu. |
| Reset przy `Restart`/reconnect | **Słusznie.** | §2.2 pkt 6, §3, test (f). |
| Audio Go2 | **Słusznie** (`manifest.toml:121`). | Zapisane w §0 i §2.1 (strumień video-only). |
| Mesh relay / frame pickup / logi bez boxów | **Słusznie.** | §6, test (b) rozszerzony; zasada logów w §2.2 pkt 5. |
| Wydajność: SGLang na tym samym GPU, brak priorytetów między procesami, kolejka wielu kamer | **Słusznie.** | Benchmark z aktywnym LLM, pula z deadline'ami (§2.2), pytanie 7. |
| CLAUDE.md: rename pliku, warianty „na wszelki wypadek", `offer_enrichment`, nadmiar ustawień | **Słusznie.** | Nazwa pliku bez zmian, w obu bundle'ach; dwa toggle, reszta stałe; brak wariantów awaryjnych. |
| Prostsze: jeden model, głowa z boxa osoby | **Słusznie** jako punkt startu, pod warunkiem pomiaru recall (ryzyko 5) i zgody użytkownika (pytanie 5). | Faza 1 = tylko YOLOv8n; SCRFD warunkowo w fazie 4. |
| Nowe pytania recenzenta | Włączone jako 4, 5, 6, 7, 9, 12; zdublowane z v1 (przerwa przy przełączaniu) usunięte, bo Branch B nie przełącza się już przy toggle blura. | |
