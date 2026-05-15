# TentaVision — plan systemu analizy obrazu z kamer

**Wersja:** v0.3 (po analizie SDK + research file)
**Forma:** **addon-aplikacja TentaFlow** (tryby: application + tools + flow blocks + service tick) korzystająca z aliasów serwisów rejestrowanych na nodach TentaFlow + Flow z FlowBuilder do orkiestracji pipeline'ów
**Deployment:** on-premise

> **§17 superseduje §2 (architektura) i §3.1 (HW)** — v0.2 framing "natywny silnik zarządzany przez addon" był ogólnie dobry, ale niezgodny z modelem SDK. v0.3 wpisuje TentaVision w istniejące mechanizmy (manifest, service registry, FlowBuilder, host functions, audit) i wprost wymienia 12 luk SDK które trzeba uzupełnić.
> Research SDK: `notes/tentavision-sdk-research.md`.
**Stylistyka UI:** komponenty `tf-*`, paleta TentaFlow (Manrope, indigo/violet, dark)

---

## 1. Problem i zakres

TentaVision analizuje strumienie z kamer IP w czasie rzeczywistym i z historii. Sześć domen analitycznych:

| # | Domena | Tryb | Krytyczność | Klasa ryzyka RODO/AI Act |
|---|--------|------|-------------|--------------------------|
| D1 | ADR — naklejki chemiczne na cysternach | real-time, brama/parking | wysoka | A (bezosobowe) |
| D2 | Anomalie zachowań (upadek, agresja, wandalizm, broń) | real-time | krytyczna | B (sylwetka, anonimowo) |
| D3 | Pozostawiony bagaż | real-time + post-event | krytyczna | A/B |
| D4 | Re-identyfikacja (twarz + person re-id; gait jako eksperyment) | real-time tylko gdy autoryzowane; historyczne pod legal gate | wysoka | **C — AI Act high-risk / Art.5 prohibited zone** |
| D5 | Wyszukiwanie po atrybutach (CLIP, tablice, marki/kolory) | post-event | średnia | B |
| D6 | Generic object detection | real-time, opt-in | niska | A |

Zasada: profil analityczny per kamera + harmonogram dzień/noc. Nie wszystko leci jednocześnie.

---

## 2. Architektura

### 2.1 Podział własności — natywny runtime + addon

```
┌─────────────────────────────────────────────────────────────┐
│                         TentaFlow                            │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  TentaVision Addon (WASM, control plane)            │   │
│  │  • Profile analityczne, harmonogramy, reguły        │   │
│  │  • Konfiguracja kamer, stref, retencji              │   │
│  │  • UI (tf-* components), eventy do flow-engine      │   │
│  │  • Polityki RODO/AI Act, gates DPIA/FRIA            │   │
│  │  • Audit policy, kontrola eksportów                 │   │
│  └────────────────────┬────────────────────────────────┘   │
│                       │ VideoAnalyticsRuntime API           │
│                       │ (handles: camera, frame, model, job)│
│                       ▼                                      │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  TentaVision Runtime (native, supervised)           │   │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────────┐ │   │
│  │  │Connectors│─▶│  Decode  │─▶│ Frame Bus (shm)  │ │   │
│  │  │RTSP/ONVIF│  │NVDEC/VA  │  │ + timestamps,    │ │   │
│  │  │Protect.. │  │API/SW    │  │   clock-sync     │ │   │
│  │  └──────────┘  └──────────┘  └────────┬─────────┘ │   │
│  │                                        ▼            │   │
│  │  ┌────────────────────────────────────────────────┐│   │
│  │  │  Shared Operators (pipeline graph engine)      ││   │
│  │  │  decoder → detector → tracker → cropper        ││   │
│  │  │           → embedder → temporal window store   ││   │
│  │  │           → event scorer → action dispatcher   ││   │
│  │  └────────────────────────────────────────────────┘│   │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────┐ │   │
│  │  │  Inference   │  │  Recording   │  │Index/Vec │ │   │
│  │  │  scheduler   │  │  ring-buffer │  │ DB       │ │   │
│  │  │  (TRT/OV)    │  │  + segmenter │  │ (Qdrant) │ │   │
│  │  └──────────────┘  └──────────────┘  └──────────┘ │   │
│  │  ┌────────────────────────────────────────────────┐│   │
│  │  │  Backpressure & QoS controller                  ││   │
│  │  │  (frame-drop policy, model degradation, alerts)││   │
│  │  └────────────────────────────────────────────────┘│   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

Hot path (decode → inference → recording → indexing) jest natywny, supervised proces z restart, health, GPU affinity. Addon WASM zarządza polityką, konfiguracją, UI, audytem, gatesami prawnymi.

### 2.2 Pipeline jako graf operatorów (nie "worker per domena")

Domeny D1-D6 są kompozycjami nad zbiorem wspólnych operatorów. Operator to typowy node grafu: konsumuje frame/tensor/event, produkuje frame/tensor/event. Operator deklaruje wymagane GPU/CPU, batch policy, latency budget.

Przykład grafu dla profilu kamery z aktywnymi D1+D3+D6:

```
RTSP → decode(NVDEC) → frame_sampler(5fps) ──┬─▶ yolo_general → tracker → bbox_sink
                                              ├─▶ yolo_adr → ocr_paddle → adr_validator
                                              └─▶ luggage_detector → owner_assoc → timer
```

Profil D2 (broń) reusuje pose graph z D2 (upadek). Jeden detektor pose = oszczędność GPU.

### 2.3 Backpressure & QoS

Pierwsza klasa obywatela, nie optimization. Polityka per kamera + per detektor:

- **Tier 0 (must run real-time):** D2 broń/agresja, alarmy ARD critical
- **Tier 1 (run real-time, degrade FPS):** D1, D3
- **Tier 2 (best-effort, can drop):** D5, D6 stats

Kontrolerzy: queue depth monitor → decision: drop frame, drop model variant, fan-in batching, lub circuit-break z alarmem operatora.

### 2.4 Komponenty wspierające (must-have, były pominięte w v0.1)

- **Camera time sync:** PTP/NTP, frame timestamps z metadanymi clock-drift.
- **Event deduplication:** ten sam event z dwóch kamer / dwóch modeli scala się przed dispatcherem.
- **Model warmup + hot reload:** model ładuje się i robi N inferencji warmup zanim wejdzie do produkcji; rollback < 60s.
- **Per-camera quality diagnostics:** brightness, blur, occlusion → degraduje pewność detektorów lub triggeruje alert "kamera brudna".
- **Health scoring:** per-kamera + per-operator (FPS rzeczywiste vs target, error rate, GPU util share).

### 2.5 `VideoAnalyticsRuntime` API (control plane → runtime)

Wąskie API z opaque handles, addon WASM nigdy nie dotyka GPU bezpośrednio.

```rust
// runtime API exposed to addon
trait VideoAnalyticsRuntime {
    // cameras
    fn add_camera(spec: CameraSpec) -> Result<CameraHandle>;
    fn remove_camera(h: CameraHandle) -> Result<()>;
    fn camera_health(h: CameraHandle) -> CameraHealth;

    // profiles (graf operatorów per kamera)
    fn apply_profile(cam: CameraHandle, profile: AnalyticsProfile) -> Result<JobHandle>;
    fn pause_job(j: JobHandle) -> Result<()>;
    fn resume_job(j: JobHandle) -> Result<()>;

    // models
    fn load_model(spec: ModelSpec) -> Result<ModelHandle>;
    fn unload_model(m: ModelHandle) -> Result<()>;
    fn rollback_model(m: ModelHandle, to_version: &str) -> Result<()>;

    // queries (post-event)
    fn search_attributes(query: AttrQuery) -> Result<Vec<Hit>>;
    fn search_reid(query: ReIdQuery, legal_grant: LegalGrant) -> Result<Vec<Hit>>; // wymaga grantu
    fn export_evidence(req: ExportRequest, grant: ExportGrant) -> Result<EvidencePackage>;

    // events (subskrypcja)
    fn subscribe_events(filter: EventFilter) -> EventStream;

    // diagnostics
    fn runtime_stats() -> RuntimeStats; // GPU, mem, queues
}
```

Każda operacja klasy C (re-id, export, wyłączenie maskowania) wymaga `LegalGrant` z aktywną podstawą prawną. Bez grantu — runtime odmawia, nie addon.

---

## 3. Modele AI per domena (state 2026)

Dla każdej domeny: rekomendacja produkcyjna + alternatywa do benchmarku + CPU fallback. Wybór per-deployment zależy od HW i datasetu.

### D1. ADR — naklejki chemiczne

| Krok | Produkcja | Benchmark / alternatywa |
|------|-----------|--------------------------|
| Detekcja pojazdu/cysterny | YOLO11m (custom fine-tune) | RF-DETR, YOLO12 |
| Detekcja tablicy ADR | YOLO11s | RT-DETR |
| OCR cyfr (UN + Kemler) | **PP-OCRv5** dla szybkiego, **PARSeq** fine-tuned dla cropped ADR digits | Tesseract (CPU fallback, słabo) |
| Czytelność/zabrudzenie | ResNet50 binarny + score | EfficientNet-B0 |
| Walidacja ADR | lokalna tabela ADR 2025 (regex + lookup) | — |

Wynik: event `adr_check { vehicle_box, un_code, kemler, hazard_class, legibility_score, photo_ref }`.

### D2. Anomalie zachowań

| Poddomena | Produkcja | Uwagi |
|-----------|-----------|-------|
| Pose + tracking (wspólny) | YOLO11-pose + BoT-SORT | wspólny operator dla wszystkich D2 |
| Upadek / zasłabnięcie | Heurystyka kątów kości + temporal model (lightweight, e.g. small TimeSformer / temporal CNN) | wymagane okno ~2s aby zbić FP |
| Agresja / bójka | **VideoMAE V2** lub **InternVideo2** fine-tuned (na zbiorach typu RWF-2000 + site-specific) | precyzja priorytet, FP <5% wymaga lokalnej kalibracji — **nie obiecywać out-of-box** |
| Broń (pistolet, nóż, długa) | YOLO11m fine-tune (datasety: WeaponS, Sohas + site-specific) | wysokie FP → **zawsze human-in-loop** confirmation flow |
| Wandalizm | klasyfikator akcji (VideoMAE V2) + change detection | często post-event |

### D3. Pozostawiony bagaż

| Krok | Produkcja |
|------|-----------|
| Detekcja bagażu | YOLO11m (COCO + ABODA + Tumult fine-tune) |
| Tracking | BoT-SORT / StrongSORT (appearance embed) |
| Powiązanie bagaż↔osoba | deterministyczne reguły geometryczne + IoU history |
| Re-id osoby (powrót) | TransReID lub CLIP-ReID (legacy OSNet jako CPU fallback) |

Konfigurowalne: próg czasu (def. 90s), strefa wykluczeń, godziny ciszy, klasy "ignored" (kosz, ławka).

### D4. Re-ID — strefa wysokiego ryzyka

**Twardy gate prawny** zanim moduł działa — szczegóły §6 i §14.

| Komponent | Produkcja | Embed size |
|-----------|-----------|-----------|
| Face detect | SCRFD-10g | — |
| Face embed | **AdaFace** (lepszy baseline na low-quality CCTV niż ArcFace/MagFace) | 512 |
| Person detect | YOLO11m + BoT-SORT | — |
| Person re-id | **TransReID** lub **CLIP-ReID** | 512–768 |
| Gait (eksperymentalne) | GaitBase z dokumentacją ograniczeń (kruche w realnym CCTV bez kontrolowanej geometrii) | 256 |

Indeks: Qdrant (HNSW). Każdy zapis: TTL, podstawa prawna, kto utworzył, expiry, link do FRIA.

### D5. Wyszukiwanie po atrybutach

| Atrybut | Model |
|---------|-------|
| Open-vocab "czerwona kurtka, czapka" | **SigLIP / SigLIP2** lub **EVA-CLIP**, plus dedykowane attribute heads dla precyzji (sam VLM halucynuje matches) |
| Detekcja zero-shot ad-hoc | Grounding-DINO |
| Tablice rejestracyjne | LPRNet / DTRB + walidator format PL/EU |
| Marka/model/kolor auta | YOLO11 + klasyfikator (VeRi-776 + Stanford Cars fine-tune) |
| Wiek/płeć szacunkowo | **WYŁĄCZONE domyślnie** (RODO/AI Act high-risk). Można włączyć tylko per-deployment z legal grant |

### D6. Generic object detection

YOLO11 (n/s/m wg HW). Custom-class support (transfer learning). Dashboard: heatmapy, liczniki, zone counts.

### 3.1 Wybór modelu wg HW

| Tier | HW | Strategia | Realne kamery (mixed) |
|------|----|-----------|-----------------------|
| Edge | Jetson Orin Nano / Intel NUC + iGPU | YOLO11n, batch=1, OpenVINO/TensorRT | 2–4 |
| Mid | 1× RTX 4070 (12GB) / 4060 Ti 16GB | YOLO11s/m, batch=4, time-slice ciężkich modeli | **~8 mixed** lub ~16 light (D1+D3+D6) |
| Pro | 1× RTX 4090 / A6000 48GB | YOLO11m/l, VideoMAE V2 dla D2 | ~24 mixed, 64 light |
| Cluster | wiele node-ów TentaFlow | load-balance przez flow-engine + GPU scheduler | bez sztywnego limitu |

Heavy combo D2 (VideoMAE V2) + D4 (face+re-id) + D5 (SigLIP) razem **łamie mid tier** bez time-slicingu i degradation. UI ostrzega "overprovisioned".

### 3.2 Budżet VRAM (mid tier, 12GB)

Liczyć nie tylko wagi, ale: TRT engines, decode surfaces (~150-300MB/kamera HEVC 1080p), batching buffers, crops queue, embedding buffers, model residency. Reasoning: dla 8 kamer 1080p HEVC tylko decode ≈ 2GB. Resztę dzielimy między modele aktywne + working set.

---

## 4. Connectory kamer

Każdy connector implementuje `CameraSource`:
```rust
trait CameraSource {
    async fn open(&mut self) -> Result<()>;
    fn frames(&self) -> impl Stream<Item = VideoFrame>;
    async fn snapshot(&self) -> Result<Image>;
    fn ptz(&self) -> Option<&dyn PtzControl>;
    fn vendor_events(&self) -> Option<impl Stream<Item = VendorEvent>>;
    fn analytics_metadata(&self) -> Option<impl Stream<Item = OnvifMetadata>>; // Profile M
    fn recording_search(&self) -> Option<&dyn RecordingSearch>; // Profile G
    fn capabilities(&self) -> CameraCapabilities;
}
```

| Vendor / Protokół | Priorytet | Notatki |
|-------------------|-----------|---------|
| RTSP universal (TCP/UDP, H.264/H.265) + HTTP snapshot fallback | **P0** | must-have |
| ONVIF Profile **S** (live) | P0 | discovery + RTSP |
| ONVIF Profile **T** (advanced streaming) | P0 | H.265, bidirectional, eventy |
| ONVIF Profile **M** (analytics metadata) | **P0** | edge analytics na kamerze (ANPR, line crossing) — nie wynalezione na nowo |
| ONVIF Profile **G** (recording/search) | **P1** | dla forensics i historicznego query |
| Hikvision ISAPI | P1 | wariancje firmware/region, ONVIF często off; ANPR onboard |
| Dahua CGI/DSS | P1 | analogicznie wariancje |
| Axis VAPIX + ACAP | P1 | wsparcie edge analytics (analiza na kamerze) |
| Hanwha (WiseNet) | P2 | enterprise, dobre eventy |
| Bosch | P2 | enterprise, IVA onboard |
| Avigilon / Motorola | P2 | enterprise rynek bezpieczeństwa |
| Milestone XProtect | P2 | jako **import source** (VMS overlay) |
| Genetec | P2 | jako import source |
| Frigate | P2 | OSS, dla migracji / co-existence |
| UniFi Protect | **P2** (zmiana z P1) | API niestabilne, pinować przetestowane wersje, RTSP fallback obowiązkowy |
| Reolink | P3 | konsumencki |
| MJPEG / HTTP push | P3 | legacy |
| File replay (mp4/mkv) | P0 | dev + forensics |

**Gotchas do wykrycia automatycznie:** firmware tier, region lock, ONVIF disabled, digest auth quirks, TLS cipher mismatch, admin-permission requirement. Wynik → UI ostrzega "kamera X: ONVIF disabled, włączymy RTSP fallback".

**Auto-discovery:** ONVIF WS-Discovery + mDNS + ARP scan. Wizard "dodaj kamerę" z auto-detect + manual fallback + capability probing.

**Recording:** hybrid policy — preferuj VMS vendora jeśli istnieje (UniFi Protect, Milestone), w przeciwnym razie własny ring-buffer (segmenty MP4 + manifest, retention per klasa detektora, GDPR-aware).

---

## 5. Funkcje aplikacji (mapa ekranów do mockupów)

Komponenty: `tf-screen`, `tf-tabs`, `tf-table`, `tf-window`, `tf-segmented`, `tf-toggle`, `tf-select`, `tf-searchbox`, `tf-chip`, `tf-button`, `tf-menu`, `tf-input`, `tf-textarea`, `tf-pin-input`.

| ID | Ekran | Cel |
|----|-------|-----|
| M1 | Dashboard | przegląd zdrowia systemu + ostatnie alarmy + heatmapa 24h |
| M2 | Live view | grid 1/4/9/16 kamer z overlay detektorów, fullscreen z timeline |
| M3 | Kamery — lista & szczegóły | tf-table + wizard "dodaj kamerę" (discovery → creds → preview → profil) |
| M4 | Profile analityczne | builder grafu operatorów per profil, przypisanie do kamer |
| M5 | Centrum alarmów | feed + filtry + karta alarmu (klip 30s, klatki, akcje, workflow potwierdzenia) |
| M6 | Wyszukiwarka historyczna | text/atrybut/podobieństwo/tablica → wyniki + eksport |
| M7 | Re-ID (D4) | dostęp przez **PIN + role + legal grant**; galeria indeksu z TTL; audit query |
| M8 | Modele i runtime | lista modeli, benchmark, rollback, ONNX upload + sanity test, budżet VRAM |
| M9 | Strefy, harmonogramy, reguły | polygon editor na kadrze, kalendarz tygodniowy, reguły AND/OR |
| M10 | Audyt + RODO | hash-chain log, retencja per klasa, generator dokumentów (DPIA, FRIA, klauzule, znaki info) |
| M11 | Eksport dla służb | paczki dowodowe (signed + TSA + HSM), authorized recipients, log eksportów |
| M12 | Ustawienia addona | storage, backendy inference, powiadomienia, licencje, integracja flow-engine |
| M13 | Onboarding wizard | rola wdrożenia → profil prawny → pierwsza kamera → presety detektorów |

---

## 6. RODO / AI Act — twarde bramy, nie tylko edukacja

Aplikacja **wymusza** podstawę prawną i workflow zatwierdzenia dla detektorów klasy C. "Educate, don't block" v0.1 zostało odrzucone — to byłaby pułapka odpowiedzialności produktowej.

### 6.1 Klasyfikacja detektorów

| Klasa | Detektory | Status RODO/EU AI Act |
|-------|-----------|------------------------|
| **A — niskie** | D1 (cysterny, bezosobowe), D3 (bagaż jako obiekt), D6 generic | RODO art. 6.1.f (uzasadniony interes) + signage; AI Act poza Annex III |
| **B — średnie** | D2 zachowania (anonimowo, sylwetka), D3 z asocjacją osoby, D5 atrybuty (bez biometrii) | DPIA wymagane; signage; krótka retencja |
| **C — wysokie / zakazane bez podstawy** | D4 face recognition, person re-id, gait, D5 wiek/płeć | **EU AI Act Annex III high-risk**. Real-time w przestrzeni publicznej **Art. 5** — zakazane poza wąskimi wyjątkami (zaginieni, terroryzm, ciężkie przestępstwa z autoryzacją sądową) |

### 6.2 EU AI Act — twarde mechanizmy w aplikacji

- **Art. 5 ban:** moduł D4 real-time w trybie "publiczna przestrzeń" wymaga konfiguracji deployment-context. Profil "Komercja prywatna" / "Lotnisko (operator)" / "Transport publiczny" / "Służby uprawnione" determinuje czy real-time D4 jest w ogóle dostępny.
- **Annex III high-risk:** dla aktywnego D4 produkt generuje automatycznie pakiet dokumentacji technicznej (art. 11 + Annex IV), post-market monitoring włączony (logi inferencji, FP/FN per kamera, fairness metrics).
- **Timeline świadomy:** prohibitions od 2.02.2025, GPAI od 2.08.2025, **Annex III obligations od 2.08.2026**. Produkt budowany teraz musi być compliant z dniem 1.

### 6.3 Mechanizmy hard-gate w UI

1. **Profil prawny przy onboardingu** (M13): Komercja prywatna / Transport publiczny (operator) / Lotnisko/dworzec / Służby uprawnione. Profil determinuje **dostępność** detektorów klasy C, nie tylko domyślne.
2. **Aktywacja detektora klasy C** = workflow modal (M7):
   - DPIA/FRIA — wymóg ukończonego dokumentu (wbudowany generator z Annex IV checklistą)
   - Podstawa prawna — dropdown z cytatem artykułu, pole sygnatury sprawy, organ wnoszący, expiry timestamp
   - Łańcuch zatwierdzeń — operator inicjuje → DPO podpisuje → osoba uprawniona zatwierdza (każdy podpis = wpis w hash-chain audit)
   - Bez ukończonego workflow detektor pozostaje **disabled na poziomie runtime**, nie tylko UI
3. **Retencja per klasa:** A:30 dni, B:14 dni, C:7 dni — z możliwością override **tylko z uzasadnieniem prawnym** wpisanym w audit
4. **Domyślne maskowanie:** twarze blur dla wszystkiego co nie jest D4 z aktywnym grantem; nawet w D4 — operator I linii widzi blur, "Uprawniony" widzi unmask
5. **Right to be forgotten:** narzędzie "usuń osobę z indeksu" + lista żądań RODO + termin realizacji
6. **Audit hash-chain:** każdy query D4/D5, każdy unmask, każdy export → append-only log + zewnętrzny WORM (osobny dysk/S3 immutable)
7. **Generator dokumentów wbudowany:** szablony klauzul informacyjnych (PL/EN), tabliczki monitoring + AI, DPIA, FRIA, wniosek o eksport dowodowy

### 6.4 "Służby" nie są magiczną rolą

Profil "Służby uprawnione" daje **dostęp do możliwości**, ale każde uruchomienie D4 real-time / każdy eksport wymaga:
- udokumentowany authority (Policja / Prokuratura / ABW / SG / inne — wybór z listy)
- numer sprawy / sygnatura postępowania
- expiry (data wygaśnięcia uprawnienia)
- podpis kierownika jednostki (lub cyfrowy odpowiednik)
- automatyczny powiadom DPO/inspektora

Brak któregokolwiek pola → runtime odmawia. Polski gap: nie polegamy na "role" w bazie — wymagamy aktywnego `LegalGrant` z TTL.

### 6.5 Wbudowane materiały referencyjne

W `addons/tentavision/legal/`:
- EU AI Act 2024/1689 (art. 5, art. 11, Annex III, Annex IV)
- EDPB Guidelines 3/2019 (video processing)
- RODO art. 6, 9, 35 (DPIA)
- Ustawa o ochronie osób i mienia
- Ustawa o Policji (art. 20 i nast.)
- KPK art. 217 (zabezpieczenie dowodów)
- ADR 2025 (tabela klas i znaków)
- Szablony DPIA, FRIA, klauzul informacyjnych

Update wraz z release.

---

## 7. Wydajność i SLO

| Metryka | Target |
|---------|--------|
| Latencja detekcja → alarm (D2 broń/agresja) | < 1.5 s p95 |
| Latencja detekcja → alarm (D1, D3, D6) | < 3 s p95 |
| FPS na kamerze (real-time mode) | 5–15 dla D1/D3/D6; 15–25 dla D2 |
| Kamery na 1× RTX 4070 (mid tier, profil mieszany) | **~8 mixed** lub ~16 light |
| Wykorzystanie GPU target | 60–80% |
| Czas wyszukiwania D5 (10M klatek indexed) | < 800 ms p95 |
| Czas re-id query D4 (100k embeddings) | < 200 ms p95 |
| Model rollback time | < 60 s |
| FP/h/kamera (target produkcyjny po kalibracji) | D2 broń <0.2, D2 agresja <0.5, D3 <0.3 |

Benchmark CLI: `tentavision bench --cameras N --profile mixed` → throughput + latencje + budżet VRAM.

---

## 8. Bezpieczeństwo

- Komunikacja z kamerami: TLS gdzie możliwe, RTSPS preferowane
- Magazyn poświadczeń: secret vault TentaFlow + **scheduler rotacji**
- **SSRF hardening:** allowlist sieci kamerowej, blok metadata endpoints (169.254.169.254, link-local, RFC1918 outside whitelist)
- **Segmentacja:** kamery w dedykowanym VLAN, runtime w innym, dashboard w trzecim — firewall między
- **Tamper-resistant audit:** append-only + hash-chain + externalizacja do WORM (S3 immutable lub osobny dysk z chattr +a)
- mTLS między TentaFlow node-ami (już istnieje)
- **Role:** viewer / operator / analyst / dpo / admin / lea-officer (Law Enforcement). Permissions matrix per ekran/akcja.
- **HSM/yubikey** dla podpisów eksportów dowodowych (sam SHA-256 to integrity, nie autentyczność). Alternatywa: TSA (RFC 3161 trusted timestamping) jako minimum.
- Anti-tamper indeksu twarzy: hash bazy w audit, alarm na unauthorized mod.

---

## 9. Roadmap implementacyjny (zmieniony — D4 za F5)

| Faza | Zakres | Kryterium zamknięcia |
|------|--------|----------------------|
| **F0** | Plan v0.2 + API gap doc + dataset/eval strategy | akceptacja, dokument `tentavision-addon-api-gaps.md` |
| **F1 — Native runtime szkielet + Live** | RTSP/ONVIF connector, decode (NVDEC/VAAPI), frame bus shm, M2 live grid, D6 (YOLO11n) | 1 kamera RTSP w UI z bboxami, runtime supervised |
| **F2 — Pipeline graph + pierwsze detektory** | shared operators graph, D1 (ADR), D3 (luggage), M5 alarm center | ADR test z 1 kamery, luggage z ABODA test set, backpressure widoczny w UI |
| **F3 — Multi-camera + profile + zones** | M3 (kamery + wizard), M4 (profile builder), M9 (zones+schedules), ONVIF Profile M | 8 kamer, profil mieszany, switch dzień/noc |
| **F4 — Search & history** | M6, recording ring-buffer, indeks atrybutów (SigLIP), ONVIF Profile G | wyszukiwanie po atrybutach na 24h nagrań |
| **F5 — D2 anomalie** | upadek, agresja, broń (VideoMAE V2 + YOLO weapons), workflow potwierdzania w M5 | 3 poddomeny D2 z site-calibrated FP <5%, eval harness uruchomiony |
| **F6 — Legal hard gates + eval harness** | M7 (re-id pod gatesem), M10 (audit+RODO), M11 (evidence + HSM/TSA), DPIA/FRIA generator | Komercja-profil blokuje D4, Służby-profil pozwala z pełnym workflow, hash-chain audit do WORM |
| **F7 — D4 wdrożenie produkcyjne** | AdaFace + TransReID + Qdrant pod legal gates z F6 | Re-id działa tylko z aktywnym `LegalGrant`, post-market monitoring włączony |
| **F8 — Vendor connectors enterprise** | Hikvision, Dahua, Axis ACAP, Hanwha, Bosch, UniFi Protect (P2), Milestone/Genetec import | 4 vendory + auto-discovery + capability probe |
| **F9 — Scale & edge** | TensorRT/OpenVINO/Jetson, dystrybucja przez flow-engine GPU scheduler | Jetson POC + 2-node cluster z load-balance |

---

## 10. Otwarte pytania

1. Vector DB: Qdrant zewnętrzny czy embedded (faiss-rs + sled) dla on-premise minimal-deps?
2. Polityka modeli broni: wbudowane wagi czy BYO-model z weryfikacją licencji? (implikacje prawne dystrybucji wag detektora broni)
3. HSM integration: kupić zależność (Yubikey HSM2 / SoftHSM dla dev) czy własny prosty TSA?
4. Czy frame bus na shm wystarczy, czy potrzebny IPC z dedykowanym schedulerem (Apache Arrow IPC?)
5. Form-factor "Służby" — osobny build z dodatkowymi feature flagami i podpisanym manifestem instalacji?

---

## 11. Dataset strategy

- **Zbieranie:** każdy deployment ma right-to-collect bucket (z opt-in od klienta + DPIA). Sample sampling per kamera per detektor.
- **Labeling:** wbudowane narzędzie w UI (M5 → "label this alarm") + integracja z Label Studio offline; podział train/val/test stratifikowany per site.
- **Negative examples:** explicit hard-negatives mining z FP alarmów (operator klika "fałszywy" → ląduje w training set).
- **Drift detection:** monthly job — porównuje rozkład embeddingów / scores z baseline; alarm DPO przy drift > threshold.
- **Per-site calibration:** każdy deployment ma własne thresholdy + adaptacja per godzina (rano/popołudnie/noc).
- **Retraining pipeline:** offline (gpu-host klienta lub sidecar), nie blokuje produkcji; nowy model → A/B shadow → przełączenie z rollback gotowym.

---

## 12. Evaluation harness

Wbudowany w runtime, uruchamiany automatycznie + on-demand:

- **Per-domain P/R/F1** na walidacyjnym secie deployment-specific
- **FP per hour per camera** (alert fatigue — krytyczna metryka)
- **Subgroup metrics** (RODO fairness): performance per płeć, wiek, oświetlenie, pora dnia
- **Latency histograms** per operator (p50/p95/p99)
- **GPU utilization breakdown** per model
- **AI Act post-market monitoring:** automatyczny raport miesięczny w formacie Annex IV — wysyłany do DPO

CLI: `tentavision eval --profile <id> --period 7d`

---

## 13. DPIA / FRIA flow (wbudowany generator)

UI flow (M7 + M10):

1. Inicjacja: operator chce aktywować detektor klasy C → modal "wymagana DPIA/FRIA"
2. Generator wypełnia automatycznie co wie (kategorie danych, kamera, retencja, model + jego ograniczenia z post-market monitoring)
3. Operator wypełnia: cel przetwarzania, podstawa prawna, oszacowanie ryzyka, środki minimalizacji
4. DPO review (in-app + email notification)
5. Podpisany dokument (cyfrowy podpis lub PDF + hash w audit)
6. Aktywacja detektora w runtime z referencją do DPIA ID
7. Reminder na review co 12 miesięcy (lub przy zmianie kontekstu)

FRIA dla AI Act = analogiczny flow z fokusem na art. 27 (Fundamental Rights Impact Assessment dla high-risk).

---

## 14. Operations

- **Upgrade path:** blue-green dla runtime, rolling update modeli z shadow inference
- **Model rollback:** każdy model ma minimum N-1 wersję na dysku, rollback < 60s przez API
- **GPU scheduler:** time-slicing per profil + per kamera, priority queues per Tier
- **Backpressure visualizer** w M8: queues, drop rate per kamera, GPU saturation
- **Failure recovery:** runtime crash → supervised restart, eventy z buforem ostatnich 60s przegrywane; runtime ↔ addon reconcile state
- **Observability:** OpenTelemetry traces + Prometheus metrics (już w TentaFlow)

---

## 15. Evidence chain (eksport dowodowy)

Paczka dowodowa = ZIP z:
- segmenty MP4 z bbox metadata
- klatki kluczowe PNG z bbox + classification
- manifest JSON (timestamps, camera_id, deployment_id, model versions, hashes)
- **podpis HSM** (Yubikey HSM2 lub SoftHSM) lub **TSA RFC 3161** (trusted timestamping) jako minimum
- legal grant + chain of approvals
- audit trail extract

Każda paczka ma UUID + wpis do `evidence_log` (append-only) + zewnętrzny backup do WORM.

Verification CLI dla strony otrzymującej: `tentavision verify package.tvevidence` → walidacja podpisu, timestampu, integralności.

---

## 16. Następne kroki

1. ✅ Plan v0.2 (ten dokument, codex feedback wbudowany)
2. ⏳ Mockupy M1–M13 w `~/.gstack/projects/Slyb00ts-TentaFlow/designs/tentavision-v1/` — komplet w jednym przebiegu
3. ⏳ Osobny dokument `tentavision-addon-api-gaps.md` — co dodać do addon API TentaFlow żeby control-plane addon zarządzał `VideoAnalyticsRuntime`
4. ⏳ Dataset strategy v0.1 jako osobny dokument (rozwinięcie §11)
5. ⏳ Eval harness spec jako osobny dokument (rozwinięcie §12)
6. ⏳ Legal pack — szablony DPIA, FRIA, klauzul (osobny katalog)

---

# §17. v0.3 — TentaVision jako addon-aplikacja TentaFlow (po analizie SDK)

Po dogłębnej analizie SDK (`notes/tentavision-sdk-research.md` + weryfikacja w kodzie `tentaflow-core/src/addon/*` i przykładach `test-app-addon`, `teams-bot`) okazało się, że v0.2 framing "natywny silnik zarządzany przez addon" był zbyt swobodny względem rzeczywistego modelu addonów. v0.3 wpisuje TentaVision w istniejące mechanizmy SDK.

## §17.1 Co TentaVision MOŻE robić jako addon (dziś)

Manifest addona deklaruje trzy + jeden tryb pracy, **wszystkie naraz**:

| Tryb | Manifest | TentaVision wykorzysta to do |
|------|----------|------------------------------|
| **Application** | `[application] entry_panel = "dashboard"` | M1–M13 ekrany (Dashboard, Live, Kamery, Profile, Alarmy, Wyszukiwarka, Re-ID, Modele, Strefy, Audyt, Eksport, Ustawienia, Onboarding) — wszystko przez `ui_render(panel_id, json_tree)` |
| **Tools (LLM)** | `[[tool]]` × N | `search_attribute`, `check_adr`, `confirm_alarm`, `run_flow`, `export_evidence` — wywołania przez agenta LLM lub przez `tool_call` z innych miejsc |
| **Flow blocks** | `blocks.json` osobny plik | `addon.tentavision.adr_check`, `addon.tentavision.luggage_check`, `addon.tentavision.action_detect`, `addon.tentavision.search_attribute` — bloki do zbudowania własnego Flow w FlowBuilder |
| **Service tick** | `[service] enabled=true` | co 1 s: refresh dashboardu, agregacja KPI, drenaż kolejki eventów, push do UI |

## §17.2 Czego TentaVision NIE robi sam (kluczowa zmiana vs v0.2)

Addon WASM **nie ma** bezpośredniego dostępu do:

- **Modeli AI / GPU** → tylko przez `service_request_call(alias, json)` do serwisów rejestrowanych na nodach
- **Ramek wideo z kamer** → ramki nie wchodzą do WASM, mieszkają w serwisie `tentavision-cam-ingest`
- **Bazy danych** → tylko `storage_get/set` (KV w core), `secret_get/set`, audit jest automatyczny
- **Zewnętrznej sieci** → tylko hosty zadeklarowane w `[[network_rule]]`; `is_safe_ip` blokuje sieci prywatne (kamery!)
- **Pliki na dysku** → brak FS API
- **Object storage** → brak (workaround: service `tentavision-blob` z S3 pod spodem)
- **Vector DB** → brak (workaround: service `tentavision-vector` z Qdrant)
- **WebSocket / real-time push** → brak; UI to req/resp + service tick

## §17.3 Architektura zgodna z SDK

```
┌─ TentaFlow core (host) ────────────────────────────────────────┐
│                                                                 │
│  ┌─ Addon TentaVision (WASM) ───────────────────────────────┐ │
│  │ Application UI (M1..M13)  · ui_render tree              │ │
│  │ Tools (LLM)               · search/check/confirm/...    │ │
│  │ Flow blocks               · adr_check, action_detect... │ │
│  │ Service tick              · 1s — refresh, agregacja     │ │
│  │ Storage KV                · konfiguracja, profile       │ │
│  │ service_request           · → aliasy modeli + serwisów  │ │
│  │ flow_invoke (ABI L10)     · uruchamia wybrany Flow      │ │
│  │ event_publish/subscribe   · alarmy, completiony         │ │
│  │ network_rule              · tylko callback webhook       │ │
│  └─────────────────────────────────────────────────────────┘ │
│                                                                 │
│  ┌─ Core: service registry, FlowBuilder, audit, perms ──────┐ │
│  │  Routuje service_request("tentavision-yolo") do          │ │
│  │  konkretnego Docker service na node-zie (mapowanie       │ │
│  │  alias→service nazwa robione przez admina przy           │ │
│  │  instalacji + zmienialne później).                        │ │
│  └─────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
            │ QUIC          │ QUIC          │ QUIC
            ▼               ▼               ▼
   ┌─ camera-      ┌─ yolo-server  ┌─ ocr-server   ...
   │  ingest      │  (Docker, GPU) │ (Docker, GPU)
   │  RTSP→frames │
   └──────────────┘
```

**Tłumaczenie aliasów:**
- W manifeście TentaVision deklaruje aliasy: `tentavision-yolo`, `tentavision-ocr`, `tentavision-action`, `tentavision-vlm`, `tentavision-face-embed` (D4), `tentavision-reid` (D4), `tentavision-recording`, `tentavision-vector`, `tentavision-evidence`, `tentavision-cam-ingest`.
- Admin TentaFlow rejestruje na nodach konkretne Docker services (z odpowiednim hardware, fallbackiem, GPU affinity).
- Przy instalacji addona admin mapuje każdy alias na konkretny service nazwę (lub akceptuje sugerowane domyślne).
- W runtime addon nie wie nic o tym co jest pod spodem — woła `service_request_call("tentavision-yolo", payload)`.

**Pipeline jako Flow:**
- Pipeline'y D1–D6 NIE są wewnętrznym grafem operatorów w UI addona.
- Pipeline = **Flow w FlowBuilder** (zewnętrzne narzędzie TentaFlow).
- TentaVision dostarcza **Flow blocks** (`addon.tentavision.adr_check`, ...) i **szablony Flow** (instalowane w `[[flow_required]]` z manifestu).
- User w UI TentaVision wybiera **który Flow** ma być wywoływany dla: real-time analizy, alarmu, eksportu dowodowego, retencji. Edycja Flow = otwarcie FlowBuilder (poza addonem).

## §17.4 Manifest TentaVision (draft v0.1, do walidacji)

Pełny draft w `notes/tentavision-sdk-research.md` §11. Najważniejsze elementy:

- `[application]` `entry_panel = "dashboard"`
- `[service]` `tick_interval_ms = 1000`
- 8 `[[permission]]`: `service.call` (medium), `flow.invoke` (medium), `storage.read/write` (low), `event.publish/subscribe` (low/medium), `secret.read` (high), `ui.render` (low)
- 10 `[[service_alias]]` (sekcja propozycyjna — luka L1): yolo, ocr, action, vlm, face-embed, reid, recording, vector, evidence, cam-ingest. Każdy ma `id`, `display_name`, `kind`, `required`, opcjonalnie `risk_class` (dla D4: C)
- 3 `[[flow_required]]` (sekcja propozycyjna — L2): `tv-realtime`, `tv-alarm`, `tv-evidence-export` z `template = "flows/*.flow.json"`
- 4–6 `[[tool]]`: search_attribute, check_adr, confirm_alarm, run_flow, export_evidence
- 2 `[[capability_gate]]` (L12): `d4-realtime`, `d4-historical` z `requires = ["dpia","fria","legal_grant","deployment_profile_lea_or_critical"]`
- 1–3 `[[ui_component]]` (L8): tv-video-grid, tv-zone-editor, tv-heatmap (custom web components addona)
- `[[network_rule]]`: tylko webhook callback do flow-engine (cała reszta przez `service_request`)
- `[gpu]` (L6): info-only — `recommended_vram_mb = 12000`
- `[config.schema]`: `default_flow_*`, `deployment_profile`, `worm_bucket`, `tsa_url`

## §17.5 Luki SDK blokujące TentaVision (do osobnego doc)

Pełna lista 12 luk w `tentavision-sdk-research.md` §10. Skrót:

| # | Luka | Rozszerzenie SDK |
|---|------|-------------------|
| L1 | brak `[[service_alias]]` w manifeście | dodać sekcję z id/display/kind/required/risk_class |
| L2 | brak `[[flow_required]]` | dodać sekcję ze szablonami Flow do instalacji |
| L3 | brak object storage API | `blob_put/get/delete` lub via service |
| L4 | brak vector DB API helpers | helpers + `service_request` kind="vector-db" |
| L5 | `is_safe_ip` blokuje sieci prywatne | flag `allow_private_ranges` w `[[network_rule]]` (admin-confirmed) |
| L6 | brak `[gpu]` info | info-only sekcja `recommended_vram_mb` |
| L7 | brak push/WS do UI | `stream_subscribe(topic)` + frontend WS bridge |
| L8 | brak custom UI components | `[[ui_component]]` registracja własnego web component'u |
| L9 | brak `risk_class` w audit | rozszerzyć `audit_log()` o pole + enum |
| L10 | brak `flow_invoke` ABI | host function `flow_invoke(flow_id, input) → run_id` + `flow_status` |
| L11 | brak `on_install` hooks z wizardem | addon zwraca multi-step tree przy instalacji |
| L12 | brak `[[capability_gate]]` | manifest opisuje "ta capability wymaga X" → core enforce |

**Decyzja:** TentaVision rusza w fazie F1 z **workaroundami** (konwencje, jeden service dla aliasów, FlowBuilder bez auto-szablonów, audit konwencyjny). Równolegle składamy PR do SDK na L1, L2, L5, L10, L12 — te są krytyczne.

## §17.6 Konsekwencje dla mockupów (M4 do przepisania + M14/M15 do dodania)

- **M4 Profile analityczne (przepisane):** profil = `{cel, FlowId, kamery, harmonogram, akcje}`. Builder grafu **zniknął**. Zamiast tego: dropdown "wybierz Flow z FlowBuilder" + przycisk "Otwórz w FlowBuilder" (link out). Tabela "Domyślne Flow per cel" + override per profil.
- **M14 (nowy) — Aliasy modeli i serwisów:** widok dla admina addona. Lista aliasów z manifestu (10 pozycji). Per alias: na jaki service zmapowane, status (ok/missing/degraded), latencja p95, fallback chain. Akcje: "Przemapuj", "Test inference", "Zobacz health".
- **M15 (nowy) — Wizard instalacji addona:** zastępuje obecny M13 onboarding. Kroki: (1) przegląd permissions z manifestu + akceptacja, (2) mapowanie 10 aliasów na konkretne service'y (autofill z service registry), (3) import 3 szablonów Flow do FlowBuilder, (4) network rules (webhook callback), (5) profil prawny (RODO/AI Act) — to zostaje z poprzedniego M13, (6) pierwsza kamera (przeniesione z poprzedniego M13).

M13 stary "Onboarding 4-krokowy" → rozbity między M15 (techniczna instalacja addona) a M13' (profil prawny — zostaje jako osobny krok wewnątrz wizarda).

## §17.7 Roadmap implementacyjny (zmieniony)

| Faza | Zakres | Kryterium |
|------|--------|----------|
| **F0** | v0.3 plan + research + 12 luk SDK | akceptacja, lista luk do PR do SDK |
| **F1 — Serwisy bazowe + addon szkielet** | service `tentavision-cam-ingest` (RTSP→frames→queue), service `tentavision-yolo` (Docker, GPU), addon WASM z 1 tool + 1 Flow block + Application skin (M1+M2 podstawowe) | 1 kamera RTSP → service → flow → bbox w UI addona |
| **F2 — Pełen łańcuch D1 + recording** | services: ocr, recording, evidence; D1 ADR jako Flow w FlowBuilder z 4 blokami; M5 Alarm Center; HSM stub | end-to-end ADR check z testowej kamery → alarm → ręczne potwierdzenie → eksport |
| **F3 — Profile + Flow selection (M4 nowy) + Kamery (M3)** | M3 wizard kamer, M4 wybór Flow, harmonogram dzień/noc, ONVIF Profile S/T/M | 8 kamer mixed profile |
| **F4 — D3 luggage + D5 search** | services: vector (Qdrant), vlm (SigLIP2); D3 i D5 jako Flow; M6 wyszukiwarka | search po atrybutach w 24h |
| **F5 — D2 anomalie + alarm workflow** | services: action (VideoMAE V2), weapons (YOLO); D2 jako Flow z human-in-loop; M5 z workflow potwierdzania | 3 poddomeny D2 z site-calibrated FP <5% |
| **F6 — Legal hard gates + capability_gate + eval harness** | M7 Re-ID gate, M10 audit+RODO, M11 evidence; SDK PR: L9 (risk_class), L12 (capability_gate); DPIA/FRIA generator | Komercja-profil blokuje D4, Służby-profil pozwala z workflow |
| **F7 — D4 produkcyjne** | services: face-embed (AdaFace), reid (TransReID); D4 jako Flow tylko pod aktywnym grantem; post-market monitoring | re-id działa tylko z `LegalGrant` |
| **F8 — SDK luki + custom UI components** | PR: L1 (service_alias), L2 (flow_required), L5 (allow_private_ranges), L8 (ui_component), L10 (flow_invoke); M14 i M15 w produkcji | manifest TentaVision w czystej formie, bez konwencji |
| **F9 — Vendor connectors enterprise** | services: hikvision-isapi, dahua-cgi, axis-vapix, unifi-protect, hanwha, bosch, milestone-import | 4 vendory + auto-discovery |
| **F10 — Scale & edge** | Jetson edge deployment, multi-node load balance, model rollback < 60s | Jetson POC + 2-node cluster |

## §17.8 Następne kroki

1. ✅ Plan v0.3
2. ⏳ Konsultacja codex — z fokusem na realność modelu aliasów + Flow + SDK luk
3. ⏳ Po akceptacji: aktualizacja mockupów — przepisany M4, nowe M14, M15 (instalacja); M13 staje się tylko "profil prawny + pierwsza kamera"
4. ⏳ Wydzielony dokument `tentavision-addon-api-gaps.md` z konkretnymi PR-ami do SDK (L1, L2, L5, L10, L12 priorytetowe)
5. ⏳ Szkielet manifest.toml jako artefakt

---

# §18. v0.3.1 — korekty po drugim review codex

Codex (consultation #2 nad §17 v0.3 i SDK research) wykrył wadę założeniową której nie złapaliśmy: **obecne `service_request_call(alias, json)` nie jest uniwersalnym RPC**. Patrząc na kod `tentaflow-core/src/addon/host_functions/service.rs:191-220` — dispatcher szuka tylko klientów inference (LLM → Embedding → TTS → STT) i pakuje payload jako `CompletionPayload`. To działa dla YOLO udającego "model", ale **NIE** dla `cam-ingest`, `recording`, `vector`, `evidence` które są semantycznie usługami non-LLM (storage, RPC, streaming).

Bez fixu — v0.3 zostaje warstwą UI nad prowizorycznymi JSON promptami, nie realnym systemem analizy wideo.

## §18.1 Ryzyko #1: typed service RPC dla non-LLM

**Decyzja:** dodać do SDK nowy primitive (zamiast nadużywać `service_request`):

```rust
// proponowane: addon-sdk/sdk/src/lib.rs
pub fn service_rpc(
    alias: &str,
    method: &str,
    payload: &[u8],
) -> Result<Vec<u8>, ServiceError>;

pub fn service_stream_open(
    alias: &str,
    method: &str,
    init: &[u8],
) -> Result<StreamHandle, ServiceError>;

pub fn service_stream_recv(h: StreamHandle, timeout_ms: u32) -> Result<Frame, ServiceError>;
pub fn service_stream_close(h: StreamHandle) -> Result<()>;
```

Core dispatcher (nowy w `tentaflow-core/src/services/runtime/`):
- registry typed-service serwisów (kind: `vision-detection`, `vision-ocr`, `storage-recording`, `vector-db`, `evidence`, `camera-source`)
- routing per kind, retry policy, GPU affinity, fallback chain
- payload nie jest opakowany w `ModelRequest` — to surowy bajt stream + method name
- audit z method, alias, kind

`service_request_call` zostaje dla LLM/STT/TTS/embed (back-compat). TentaVision idzie wyłącznie po nowym `service_rpc`/`service_stream_*`.

**To jest nowa luka L13** — najwyższy priorytet, blokuje wszystko poza Application UI.

## §18.2 Aliasy — resolver capability-matching, nie mapa string→string

Codex: "prosta mapa wystarczy na demo, rozpadnie się przy 10 aliasów × N profili × M nodów". Akceptuję.

**Zamiana modelu z §17.4:**
- W manifeście addon NIE deklaruje "aliasów" jako konkretnych nazw serwisów. Deklaruje **wymagane capabilities**:

```toml
[[required_capability]]
id = "vision.detect"
display_name = "Detekcja obiektów na kadrach"
required = true
inputs = ["frame_ref"]
outputs = ["detections[]"]
constraints = { min_throughput_fps = 5, max_latency_ms = 800, supports_classes = ["truck","person","luggage"] }

[[required_capability]]
id = "vision.ocr"
display_name = "OCR cyfr ADR / tablic rejestracyjnych"
required = true
constraints = { domain = ["adr_digits","license_plate_pl_eu"] }

[[required_capability]]
id = "vision.action"
required = false
data_class = "B"

[[required_capability]]
id = "vision.face_embed"
required = false
data_class = "C"
gate = "d4-historical"   # nie aktywne dopóki capability_gate niezaspokojony

[[required_capability]]
id = "video.recording"
kind = "storage"
constraints = { ring_buffer = true, signed_segments = true }
```

- Core ma **resolver** który dla każdej `required_capability` (per profil / per kamera / per node) wybiera najlepszy zarejestrowany service na podstawie deklarowanych capabilities + polityki (latency, lokalizacja, GPU, koszt).
- User dostaje UI mapowania **z preview** — "TentaVision wymaga vision.detect; w deployment masz 3 kandydatów: yolo11m@node-A (latency 12ms, GPU OK), yolo11s@node-B (45ms), rf-detr@node-A (8ms, GPU 90% busy)". User wybiera + ustawia fallback.
- Mapowanie zapisane jako binding `(addon × capability × context) → service`. Edytowalne, audytowalne.

To zmienia M14 z "lista aliasów + status" na "explorer capabilities + binding builder".

## §18.3 Streaming + backpressure — kontrakt media/event

Codex: "FlowBuilder nadaje się do orkiestracji decyzji i alarmów, ale nie do hot path wideo. Te elementy muszą zostać first-class w `cam-ingest`/video runtime service, a Flow powinien widzieć stabilne eventy/frame refs, nie pojedyncze ramki".

**Konkretnie:**
- `tentavision-cam-ingest` service (per-node) ekspozuje typed streamy: `frame_refs(camera_id) → Stream<FrameRef>`, `events(camera_id) → Stream<CameraEvent>`. `FrameRef` to opaque uchwyt z metadanymi (timestamp PTP, camera_id, sequence_num, resolution), **nie sama ramka**.
- Bbox stream / detection events / action events to typed message types, nie JSON-y.
- Backpressure (drop frame, degrade FPS, circuit break) jest własnością service'u — nie wycieka do Flow ani do addona. Flow widzi **stabilne eventy**: "ADR check completed", "agresja confirmed", "luggage unowned >90s", z metadanymi (camera, time, frame_refs).
- Time-sync (PTP/NTP), warmup, hot reload, rollback < 60s — first-class w video runtime service, nie w addonie ani w Flow.

To jest **L14 (nowa luka)**: typed streaming primitive w SDK addonów + protokole QUIC do video runtime.

## §18.4 Capability gates — generic, nie domenowe

Codex: "Core nie powinien znać DPIA/FRIA jako twardych enumów. Core powinien znać generyczny policy primitive: grant, approval, expiry, scope, audit evidence, required claims".

**Konkretnie:** zmieniam manifest TentaVision z `[[capability_gate]]` (specific dla TentaFlow) na deklarację wymaganych **claims**:

```toml
[[gate]]
id = "d4-realtime"
display_name = "D4 real-time re-identyfikacja"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "approval", subject = "fria", status = "signed" },
  { type = "grant", scope = "biometric:realtime", valid = true, has_expiry = true },
  { type = "deployment_profile", oneof = ["lea","critical_infra"] },
]
```

Core ma generyczny policy engine: rejestruje claims, sprawdza expiry, propaguje do audit, blokuje host function jeśli gate niezaspokojony. TentaVision (i inne high-risk addony) wpinają się przez deklarację required_claims. DPIA/FRIA są semantykami TentaVision, nie core.

To rafinuje L12 — to nie jest "TentaVision-specific gate", to jest **generic policy + claims store w core** (L12 ponownie zdefiniowane).

## §18.5 Kamera jako resource (nie service)

Codex: "Nie robiłbym jednej kamery jako jednego service registry entry w MVP. Najlepszy kompromis: jeden `cam-ingest` per node, a kamery jako first-class child resources z własnym health, shard key, failover policy".

**Model:**
- Service `tentavision-cam-ingest` (jeden per node).
- Każda kamera = **resource** zarejestrowany w core resource registry (nowy concept lub rozszerzenie existing). Ma: id, vendor, credentials (secret_ref), node-binding (który ingest service ją obsługuje), retention, health, ownership (kto może oglądać), audit_scope.
- Migracja kamery między nodami = przepięcie resource pointer'a; ingest service przejmuje stream.
- To jest **L15 (nowa luka)**: camera-as-resource model.

## §18.6 Pipeline w UI TentaVision — embedded profile view

Codex: "Operator nie będzie otwierał DAG żeby zmienić próg bagażu z 90s na 120s. TentaVision UI powinien mieć embedded profile view dla parametrów wysokiego poziomu".

**Praktyka:** M4 ma dwa poziomy:
- **Quick params** (większość userów): formularz "ADR legibility threshold = 0.65 / luggage no-owner = 90s / aggression FP guard = 0.75". Zapisuje overrides w konfiguracji addona, runtime service je czyta przy każdym wywołaniu Flow.
- **Advanced** (link out): "Otwórz Flow w FlowBuilder" — pełna edycja DAG.

Override-y propagują się jako **input parameters do Flow** (Flow ma deklarowane params, addon je zna z manifestu Flow templates).

## §18.7 Custom UI components — twardy sandbox

Codex: "Shadow DOM izoluje style, nie izoluje bezpieczeństwa. Rekomendacja: podpisane komponenty, strict CSP, brak inline/eval, ograniczony bridge API, iframe sandbox dla high-risk".

**Polityka L8 (przepisana):**
- Manifest deklaruje `[[ui_component]]` z `src`, `slot`, `signature` (Ed25519 podpis bundle JS), `risk = low|medium|high`.
- Komponenty `low` → ładowane do shadow DOM dashboardu (sandboxowane przez CSP `script-src 'self'`, brak eval, brak inline, allowlist API przez `postMessage` bridge).
- Komponenty `high` (video grid z plain video, polygon editor z access do raw obrazu) → ładowane w **iframe sandbox** z `sandbox="allow-scripts"` (bez allow-same-origin) + `postMessage` bridge do core. Brak dostępu do DOM rodzica.
- Bridge API: enumerowane "operations" (get_panel_state, set_value, emit_event), bez free-form access.
- Bez signatures → instalacja odrzucona.

## §18.8 Flow templates — bez auto-install

Codex: "Auto-installacja bez zgody jest zła. Flow może mieć skutki operacyjne i prawne".

**Decyzja:** `[[flow_required]]` przepisane na `[[flow_template]]` (templates only):
- Manifest deklaruje szablony.
- Wizard instalacji addona pokazuje listę templates z preview (jakie bloki, jakie inputs, jakie capabilities są wywoływane).
- User świadomie importuje (lub odrzuca / modyfikuje). Każdy import audytowany.
- TentaVision nadal może wskazać "rekomendowane domyślne Flow", ale to user/admin podejmuje decyzję.

## §18.9 Zaktualizowana lista luk SDK (po §18)

Skonsolidowana, zaktualizowana lista. Stary numer w nawiasie:

| # | Luka | Priorytet | Komentarz |
|---|------|-----------|-----------|
| **L1 (L1)** | `[[required_capability]]` w manifeście (zastąpienie `[[service_alias]]`) | **F1 BLOCKER** | resolver capability-matching, nie mapa string |
| **L2 (L13 nowa)** | Typed service RPC + streaming (`service_rpc`, `service_stream_*`) dla non-LLM | **F1 BLOCKER** | bez tego cam-ingest/recording/vector niedostępne |
| **L3 (L14 nowa)** | Media/event streaming contract z backpressure (FrameRef, detections stream, ordering) | **F1 BLOCKER** | hot path wideo |
| **L4 (L15 nowa)** | Camera-as-resource model w core registry | F2 | one ingest service, wiele kamer-resources |
| **L5 (L8)** | Custom UI components z podpisem + iframe sandbox dla high-risk | F2 | bogate UI TentaVision |
| **L6 (L10)** | `flow_invoke` ABI + `flow_status` | F2 | uruchamianie wskazanych Flow |
| **L7 (L11)** | `on_install(ctx)` z multi-step wizard | F2 | instalacja TentaVision |
| **L8 (L12 redefined)** | Generic policy engine z claims store w core | F3 | gates jako data, nie enum |
| **L9 (L9)** | `risk_class` / `data_class` w audit | F3 | klasyfikacja |
| **L10 (L7)** | Real-time push do UI (event stream → frontend) | F3 | live updates dashboardu |
| **L11 (L2 — Flow templates)** | `[[flow_template]]` (templates only, opt-in install) | F3 | bezpieczeństwo |
| **L12 (L3)** | Object storage API (`blob_*`) lub przez recording service | F3 | klipy 30s, snapshoty |
| **L13 (L6)** | `[gpu]` info-only sekcja w manifeście | F4 | tylko info |
| **L14 (L5)** | `allow_private_ranges` w network_rule | nie dla TentaVision (wyrzucamy ingest poza addon) | dla innych use-case |

L1–L3 są blokery dla MVP. Bez nich TentaVision = tylko Application UI bez wartości.

## §18.10 Zaktualizowane mockupy

- **M4 (przepisać):** Quick params (threshold sliders) + link "Otwórz Flow w FlowBuilder" + "Pokaż templates Flow z manifestu addona"
- **M14 (zmieniony):** **Capability bindings** — explorer wymaganych capabilities (z manifestu) + lista zarejestrowanych services w deployment + przypisanie + fallback chain + health
- **M15 (nowy wizard instalacji):** krok 1 permissions, krok 2 **capability bindings** (nie aliasy), krok 3 review flow templates (opt-in), krok 4 network rules, krok 5 profil prawny, krok 6 pierwsza kamera (jako resource, nie service entry)

M13 stary (4-krokowy onboarding) → zlikwidowany, treść rozdzielona między M15 (techniczna instalacja) i osobny krok "profil prawny" wewnątrz wizarda.

## §18.11 Wniosek

v0.3.1 odsuwa od planu "addon z aliasami modeli" i wpisuje TentaVision w model **addon-aplikacja deklarująca capabilities + Core dostarcza policy + Core ma typed service RPC + streaming + claims**.

Plan w kolejnych krokach:
1. Wytypować PR do SDK (L1, L2, L3) — to są blokery, bez nich F1 niemożliwe.
2. `tentavision-addon-api-gaps.md` jako odrębny dokument z draftem ABI dla L1–L3.
3. Mockupy: zaktualizować M4, dodać M14 capability bindings, M15 install wizard. Stare mockupy M1, M2, M3, M5–M13 zostają (z drobną korektą breadcrumb / tabs jeśli M14/M15 dodają nowe taby).
4. Manifest TentaVision draft v0.2 (z `[[required_capability]]`, `[[gate]]` claims, `[[ui_component]]` z signaturą, `[[flow_template]]`).

---

# §19. v0.3.2 — uproszczone API SDK (po feedback usera)

User skorygował przekombinowanie codex (capability-matching resolver) na rzecz prostszego modelu zgodnego z istniejącym wzorcem `service_request_call` (teams-bot). Klucze:

- **Aliasy = TYLKO AI services**. Nie nadużywać tego mechanizmu pod storage/recording/evidence.
- **Storage / vector / recording / evidence / camera** = osobne **wbudowane API core** które musimy dopisać do SDK.
- Pull-based streaming z kamer (addon w `on_tick` woła `stream_next`).
- Kamery są zarządzane przez TentaVision ale są zasobem globalnym — inne addony mogą się podpiąć z odpowiednim permission.

## §19.1 Wbudowane API w SDK (do dodania)

### A. AI services przez aliasy

Rozszerzenie istniejącego `service_request` o `method` (dziś payload pakowany w `CompletionPayload`):

```rust
service_call(alias: &str, method: &str, payload: &[u8]) -> Result<Vec<u8>>
```

Aliasy zadeklarowane w manifeście (`[[service_alias]]`), admin przy instalacji mapuje na konkretny zarejestrowany service. Brak resolvera, brak capability matching.

Aliasy TentaVision:
| Alias | Metody |
|-------|--------|
| `tentavision-yolo` | `detect`, `track` |
| `tentavision-ocr` | `recognize`, `recognize_cropped` |
| `tentavision-action` | `classify_window` |
| `tentavision-vlm` | `embed`, `caption` |
| `tentavision-face-embed` | `embed` (D4) |
| `tentavision-reid` | `embed`, `match` (D4) |

### B. SQL API (nowe — per-addon SQLite + opcjonalny PostgreSQL)

Obecny `storage_get/set` (KV w wspólnej `addon_storage` tabeli) → zachowujemy back-compat, ale **dodajemy** prawdziwe SQL:

```rust
sql_exec(query: &str, params: &[Value]) -> Result<u64>
sql_query(query: &str, params: &[Value]) -> Result<Vec<Row>>
sql_query_one(query: &str, params: &[Value]) -> Result<Option<Row>>
sql_transaction(stmts: &[(&str, &[Value])]) -> Result<()>
```

Backend:
- **SQLite default:** dedykowany plik per addon `~/.tentaflow/addons/<addon_id>/data.db`. Fizyczna izolacja przez FS sandbox w core. Migrations: katalog `migrations/` w bundle addona, core uruchamia uporządkowane przy install/upgrade.
- **PostgreSQL opcjonalny:** addon deklaruje `[database] backend = "postgres"`. Admin konfiguruje connection per addon (osobne database lub schema, role ograniczona). Core proxy-uje wszystkie zapytania (audit + injection guard).

Manifest:
```toml
[database]
backend = "sqlite"
migrations_dir = "migrations"
encryption = "at-rest"   # opcja: SQLCipher dla SQLite, pgcrypto dla PG
```

### C. Vector store API (nowe w core — namespaced)

Backend MVP: embedded HNSW (`hnsw_rs` / `instant-distance`) z persystencją do plików w `~/.tentaflow/vector/<addon_id>/<namespace>.hnsw`. Opcjonalnie później Qdrant jako external service.

```rust
vector_upsert(namespace: &str, items: &[VectorItem]) -> Result<()>
vector_search(namespace: &str, query: &[f32], k: usize, filter: VectorFilter) -> Result<Vec<Hit>>
vector_delete(namespace: &str, ids: &[String]) -> Result<()>
vector_count(namespace: &str) -> Result<u64>

struct VectorItem { id: String, vector: Vec<f32>, metadata: Map<String, Value> }
struct Hit { id: String, score: f32, metadata: Map<String, Value> }
struct VectorFilter { /* metadata predicates */ }
```

Manifest namespace deklaracja:
```toml
[[vector_namespace]]
name = "faces"
dimensions = 512
distance = "cosine"
data_class = "C"          # propagowane do retencji + audit

[[vector_namespace]]
name = "attributes"
dimensions = 768
distance = "cosine"
```

Vector store **też dla RAG** — przyszłe use cases nie tylko TentaVision.

### D. Recording API (nowe w core — clip_ref opaque)

Addon **nigdy** nie widzi ścieżek do plików nagrań. Core ma własny moduł recording z ring-bufferem per kamera, retencją, oraz API:

```rust
recording_save_segment(camera_id, start_ts, end_ts) -> Result<ClipRef>
recording_save_snapshot(camera_id, ts) -> Result<SnapshotRef>
recording_get_stream(clip_ref) -> Result<StreamHandle>     // do live playback / download
recording_get_url(clip_ref, ttl_sec) -> Result<Url>        // signed URL dla frontendu
recording_purge(clip_ref) -> Result<()>                    // honoruje retention policy
recording_stats(camera_id) -> Result<RecordingStats>       // disk usage, oldest segment
```

Addon w swojej SQL bazie trzyma tylko `clip_ref` + metadane (kamera, czas, alarm_id, hash). Wszystko inne — w core. Inne addony mogą wołać `recording_get_*` jeśli mają permission `recording.read` i znają clip_ref (alarm-id mapping w SQL bazie TentaVision).

### E. Evidence API (nowe w core — HSM/TSA wbudowane)

```rust
evidence_sign(payload: &EvidencePayload) -> Result<SignedPackage>
evidence_verify(package: &SignedPackage) -> Result<VerifyResult>
evidence_anchor(package_id: &str) -> Result<AnchorRef>   // blockchain anchor opt

struct EvidencePayload {
    clip_refs: Vec<ClipRef>,         // z recording API
    snapshots: Vec<SnapshotRef>,
    manifest_json: String,           // addon-supplied (legal_grant, case_no, ...)
}
struct SignedPackage {
    id: String,
    bundle_url: String,              // signed URL do pobrania ZIP-a
    signature: Vec<u8>,
    timestamp_token: Vec<u8>,
    chain_hash: String,
}
```

Core obsługuje HSM (Yubikey HSM2 / SoftHSM), TSA (RFC 3161), opcjonalnie blockchain anchoring. Konfigurowalne globalnie w TentaFlow (per deployment) — nie per addon. Wszystkie wywołania są audytowane jako klasa C.

### F. Camera API (nowe w core — managed by addon, owned by core)

TentaVision zarządza kamerami z poziomu addona, ale fizyczny ingest RTSP/ONVIF/Protect żyje w core (osobny moduł lub natywny serwis `tentaflow-camera-ingest`).

```rust
camera_add(spec: CameraSpec) -> Result<CameraId>
camera_list(filter: CameraFilter) -> Result<Vec<CameraInfo>>
camera_get(id: CameraId) -> Result<CameraInfo>
camera_update(id: CameraId, patch: CameraPatch) -> Result<()>
camera_remove(id: CameraId) -> Result<()>
camera_snapshot(id: CameraId) -> Result<ImageRef>        // jednorazowy obraz
camera_credentials_rotate(id: CameraId) -> Result<()>

struct CameraSpec {
    vendor: Vendor,                      // RtspGeneric | Onvif | UnifiProtect | Hikvision | ...
    location: String,
    url_or_host: String,
    credentials_secret_ref: SecretRef,   // addon zapisuje przez secret_set, dostaje ref
    retention_class: RiskClass,
    ownership: AddonId,                  // ten addon jest właścicielem
    shared_with: Vec<AddonId>,           // inne addony które mogą czytać
    ...
}
```

Poświadczenia kamer **nigdy** nie idą przez addon WASM jako plaintext — addon woła `secret_set("cam-X-creds", encrypted_blob)`, dostaje `SecretRef`, podaje ref do `camera_add`. Core trzyma sekret, kamery-ingest service używa go bezpośrednio.

### G. Streaming API (nowe w core — pull-based)

```rust
stream_subscribe(target: StreamTarget, filter: StreamFilter) -> Result<StreamId>
stream_next(id: StreamId, timeout_ms: u32) -> Result<Option<StreamMessage>>
stream_close(id: StreamId) -> Result<()>

enum StreamTarget {
    Camera { id: CameraId, sample_fps: Option<u32> },
    DetectorEvents { profile_id: ProfileId },
    EventBus { topic_pattern: String },
}

enum StreamMessage {
    Frame { camera_id, ts, frame_ref: FrameRef, sequence: u64 },
    Event { camera_id, ts, kind: String, payload: Vec<u8> },
    Detection { camera_id, ts, frame_ref, boxes: Vec<Bbox> },
    End { reason: String },
}
```

`FrameRef` jest **opaque uchwytem** — addon nie ma dostępu do bajtów, ale może przekazać `frame_ref` do `service_call("tentavision-yolo", "detect", {frame_ref})` i serwis YOLO (zarejestrowany na nodzie z GPU) sobie ramkę pobierze z core po referencji.

Backpressure: jeśli addon nie drenuje streamu szybko, core dropuje najstarsze wiadomości i raportuje w `stream_next` (`StreamMessage::Drop { count }`).

### H. Camera ownership / sharing

User wybrał: TentaVision jest właścicielem kamer, ale są one zasobem globalnym. Inne addony mogą czytać po nadaniu permission. Permission `camera.read` na poziomie addon → addon. UI w TentaFlow Administration: "TentaVision udostępnia 22 kamery dla: AccessControl-addon, MeetingBot-addon (snapshot only)".

## §19.2 Skonsolidowana lista API gaps SDK (ostateczna)

Stary numer w nawiasie. Priorytety: **F1** = blocker MVP, F2-F3 = enrichment.

| # | API | Priorytet | Status SDK |
|---|-----|-----------|-----------|
| API-1 | `service_call(alias, method, payload)` — rozszerzenie istniejącego service_request | **F1** | rozszerzenie istniejącego mechanizmu |
| API-2 | `[[service_alias]]` w manifeście | **F1** | nowe pole manifestu |
| API-3 | SQL API (`sql_exec/query/transaction`) + per-addon SQLite | **F1** | nowy KV → SQL |
| API-4 | Manifest `[database] backend/migrations` + bootstrap migrations | **F1** | nowy |
| API-5 | Camera API (`camera_add/list/snapshot/...`) | **F1** | nowy moduł core |
| API-6 | Streaming API (`stream_subscribe/next/close`) + FrameRef opaque | **F1** | nowy moduł core |
| API-7 | Vector API (`vector_upsert/search/delete`) + manifest `[[vector_namespace]]` | F2 | nowy moduł core |
| API-8 | Recording API (`recording_save_segment/get/purge`) + ring-buffer manager | F2 | nowy moduł core |
| API-9 | Evidence API (`evidence_sign/verify/anchor`) + HSM/TSA integracja | F2 | nowy moduł core |
| API-10 | `[[ui_component]]` z signaturą + iframe sandbox dla high-risk | F2 | nowy mechanizm UI |
| API-11 | `flow_invoke(flow_id, input) → run_id` + `flow_status` | F2 | nowa host fn |
| API-12 | `[[flow_template]]` (opt-in install) | F2 | nowy manifest |
| API-13 | Audit `risk_class` enum + per-call tagging | F3 | rozszerzenie audit |
| API-14 | `on_install(ctx)` z multi-step wizard | F3 | nowy lifecycle hook |
| API-15 | Generic policy/claims engine (gates) | F3 | nowy moduł core |

API-1 do API-6 są **blockerami** dla MVP TentaVision (F1). API-7 do API-9 są krytyczne dla F2-F3 (rzeczywiste use-case TentaVision: search, recording, evidence). API-10 do API-15 wzbogacenia.

## §19.3 Manifest TentaVision draft v0.3 (po §19)

Kluczowe różnice vs §17.4:

```toml
[addon]
id = "tentavision"
# ...

[application]
entry_panel = "dashboard"
title = "TentaVision"

[service]
enabled = true
tick_interval_ms = 250         # szybszy niż domyślny — drenaż streamów

# === Database (nowe) ============================================
[database]
backend = "sqlite"
migrations_dir = "migrations"

# === Vector namespaces (nowe) ===================================
[[vector_namespace]]
name = "attributes"
dimensions = 768
distance = "cosine"
data_class = "B"

[[vector_namespace]]
name = "faces"
dimensions = 512
distance = "cosine"
data_class = "C"
gate = "d4-historical"

[[vector_namespace]]
name = "persons"
dimensions = 512
distance = "cosine"
data_class = "C"
gate = "d4-historical"

[[vector_namespace]]
name = "plates"
dimensions = 256
distance = "cosine"
data_class = "B"

# === AI service aliases — TYLKO AI =============================
[[service_alias]]
id = "tentavision-yolo"
display_name = "Detektor obiektów"
methods = ["detect", "track"]
required = true

[[service_alias]]
id = "tentavision-ocr"
methods = ["recognize", "recognize_cropped"]
required = true

[[service_alias]]
id = "tentavision-action"
methods = ["classify_window"]
required = false

[[service_alias]]
id = "tentavision-vlm"
methods = ["embed", "caption"]
required = false

[[service_alias]]
id = "tentavision-face-embed"
methods = ["embed"]
required = false
gate = "d4-historical"

[[service_alias]]
id = "tentavision-reid"
methods = ["embed", "match"]
required = false
gate = "d4-historical"

# === Permissions ==============================================
[[permission]]
id = "service.call"
risk = "medium"

[[permission]]
id = "camera.manage"
display_name = "Zarządzaj kamerami (add/remove/configure)"
risk = "medium"

[[permission]]
id = "camera.read"
risk = "low"

[[permission]]
id = "stream.subscribe"
risk = "medium"

[[permission]]
id = "sql.read"
risk = "low"

[[permission]]
id = "sql.write"
risk = "low"

[[permission]]
id = "vector.read"
risk = "low"

[[permission]]
id = "vector.write"
risk = "low"

[[permission]]
id = "recording.save"
risk = "medium"

[[permission]]
id = "recording.read"
risk = "medium"

[[permission]]
id = "evidence.sign"
risk = "high"
gate = "deployment_profile_lea_or_critical"

# ... (audit/flow.invoke/secret/event jak wcześniej)

# === Flow templates (opt-in install) ==========================
[[flow_template]]
id = "tv-realtime-adr"
path = "flows/tv-realtime-adr.flow.json"

[[flow_template]]
id = "tv-alarm-enrich"
path = "flows/tv-alarm-enrich.flow.json"

[[flow_template]]
id = "tv-evidence-export"
path = "flows/tv-evidence-export.flow.json"

# === Gates (claims-based) ====================================
[[gate]]
id = "d4-historical"
display_name = "Re-identyfikacja historyczna (D4)"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "grant", scope = "biometric:historical", valid = true, has_expiry = true },
]

[[gate]]
id = "d4-realtime"
display_name = "Re-identyfikacja w czasie rzeczywistym"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "approval", subject = "fria", status = "signed" },
  { type = "grant", scope = "biometric:realtime", valid = true, has_expiry = true },
  { type = "deployment_profile", oneof = ["lea", "critical_infra"] },
]

[[gate]]
id = "deployment_profile_lea_or_critical"
required_claims = [
  { type = "deployment_profile", oneof = ["lea", "critical_infra"] },
]
```

## §19.4 Mockup M14 (przepisany — prosta mapa aliasów)

Zamiast capability bindings explorer:
- Tabela **AI aliasów** (6 wierszy, tylko AI): alias · zmapowany service · node · latency · status · fallback
- Sekcja "Camera management" (statystyki tab API): N kamer zarządzanych, N gotowych do streamingu, ostatni problem
- Sekcja "Storage usage" (SQL + vector): SQLite size, namespaces wektorów, recording disk usage

Bez "capabilities", bez resolvera. Admin po prostu mapuje 6 aliasów AI na zarejestrowane services.

## §19.5 Następne kroki (po §19)

1. ✅ §19 plan v0.3.2 — uproszczone SDK API
2. ⏳ Mockup M14 (przepisany — prosta mapa aliasów AI + sekcje wbudowanych API)
3. ⏳ Mockup M15 (install wizard — z mapowaniem 6 aliasów + permissions + Flow templates)
4. ⏳ Osobny dokument `tentavision-addon-api-gaps.md` — pełne RFC dla 15 API (z draftem ABI dla każdego)
5. ⏳ Decyzja: czy migrować obecny storage_* (KV) na SQL API, czy zostawić oba (KV jako fast-path)

---

# §20. v0.3.3 — finalna decyzja storage (KV + SQL z wyborem backendu)

## §20.1 Model storage w SDK (final)

**Dwa niezależne systemy storage, każdy opt-in:**

### KV (zostaje jak dziś)
- Backend: wspólna baza TentaFlow, tabela `addon_storage(addon_id, instance_id, key, value)`
- API: `storage_get/set/delete/list` (już istnieje)
- Limity: 1 MB/value, 10 000 kluczy/addon, klucz max 1024 B
- Use case: config, flagi, liczniki, last-seen — proste mapy
- Zero migracji istniejących addonów

### SQL (nowy)
- Backend wybierany przy instalacji: **SQLite** (per-addon plik) lub **PostgreSQL** (osobny database)
- API: `sql_exec`, `sql_query`, `sql_query_one`, `sql_transaction`
- Use case: relacyjne dane, query po indeksach, joiny, time-series
- Migracje z bundle addona (`migrations/` w archiwum)

### Manifest deklaracja

```toml
[storage]
kv = true                                    # default true
sql = true                                   # default false
sql_backends = ["sqlite", "postgres"]        # required jeśli sql=true
sql_dialect = "ansi"                         # ansi | sqlite | postgres
migrations_dir = "migrations"
```

### Reguły:
- `sql_dialect = "ansi"` → addon obiecuje pisać podzbiór ANSI SQL, runtime weryfikuje przy load migrations (opcjonalnie z lintera). Działa na obu backendach.
- `sql_dialect = "sqlite"` lub `"postgres"` → addon wymaga konkretnego backendu, `sql_backends` musi zawierać tylko ten.
- Jeśli manifest deklaruje `sql_backends = ["sqlite", "postgres"]` i dialect `"ansi"` → admin przy instalacji wybiera (default: SQLite jako prostsze).

### SQLite per-addon
- Plik: `~/.tentaflow/addons/<addon_id>/data.db`
- WAL mode włączony domyślnie
- Sandbox FS: addon nigdy nie dostaje ścieżki, tylko handle przez `sql_*`
- Backup: kopia pliku (core ma `addon_backup_create(addon_id)` w admin API)
- Encryption opcjonalna: SQLCipher gdy `encryption = "at-rest"`

### PostgreSQL per-addon
- Osobny **database** (nie schema): `tentaflow_addon_<addon_id>`
- Osobny user/role: `tentaflow_addon_<addon_id>` z dostępem tylko do tego database
- Admin TentaFlow przy instalacji podaje:
  - Connection string (host, port, admin credentials do `CREATE DATABASE`)
  - Lub: pre-created database + user/password (gdy admin sam tworzy)
- Core trzyma per-addon pool połączeń, addon WASM idzie przez core proxy z audit + injection guard
- Migracje uruchamiane przez core jako addon-user (nie admin)

### Decyzja dla TentaVision

```toml
[storage]
kv = true                                    # do prostych config
sql = true
sql_backends = ["sqlite", "postgres"]
sql_dialect = "ansi"                         # przenośność
migrations_dir = "migrations"
```

Schemat SQL (ANSI subset, działa na SQLite + Postgres):

```sql
-- migrations/001_init.sql
CREATE TABLE cameras (
  id            TEXT PRIMARY KEY,
  vendor        TEXT NOT NULL,
  url           TEXT NOT NULL,
  credentials_ref  TEXT,
  location      TEXT,
  retention_class  TEXT NOT NULL,
  ownership     TEXT NOT NULL,
  shared_with   TEXT,         -- JSON array as text dla ANSI compat
  added_at      INTEGER NOT NULL,
  last_seen     INTEGER
);
CREATE INDEX idx_cameras_vendor ON cameras(vendor);

CREATE TABLE profiles (
  id            TEXT PRIMARY KEY,
  flow_id       TEXT NOT NULL,
  schedule      TEXT,
  retention     TEXT,
  data_class    TEXT NOT NULL,
  active        INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE alarms (
  id            TEXT PRIMARY KEY,
  ts            INTEGER NOT NULL,
  camera_id     TEXT NOT NULL,
  detector      TEXT NOT NULL,
  confidence    REAL,
  status        TEXT NOT NULL,
  clip_ref      TEXT,
  operator_id   TEXT,
  notes         TEXT
);
CREATE INDEX idx_alarms_ts ON alarms(ts);
CREATE INDEX idx_alarms_camera_ts ON alarms(camera_id, ts);

CREATE TABLE recordings_meta (
  clip_ref      TEXT PRIMARY KEY,
  camera_id     TEXT NOT NULL,
  start_ts      INTEGER NOT NULL,
  end_ts        INTEGER NOT NULL,
  hash          TEXT,
  alarm_id      TEXT
);

CREATE TABLE legal_grants (
  id            TEXT PRIMARY KEY,
  authority     TEXT NOT NULL,
  case_no       TEXT NOT NULL,
  expiry        INTEGER NOT NULL,
  scope         TEXT NOT NULL,
  dpo_signature TEXT,
  issued_at     INTEGER NOT NULL
);
CREATE INDEX idx_grants_expiry ON legal_grants(expiry);
```

Plus tabele dla audytu lokalnego (replika krytycznych zdarzeń), recordings_index, ekspów dowodowych itd. — wszystko w tym samym pliku/DB.

## §20.2 Konsekwencje dla mockupów

- **M14 (przepisywany):** sekcja "Storage" pokazuje:
  - KV: liczba kluczy / 10 000, rozmiar
  - SQL: backend (SQLite albo PostgreSQL), rozmiar bazy, liczba rekordów per tabela
  - Vector: namespaces (`faces`, `attributes`, ...) z liczbą wektorów i rozmiarem
  - Recording: GB użyte, segmenty per kamera, najstarszy klip
- **M15 install wizard:** nowy krok "Storage":
  - Pokazuje co addon deklaruje: KV ✓, SQL ✓ (backends: SQLite / PostgreSQL)
  - Admin wybiera SQL backend
  - Jeśli PostgreSQL — formularz connection (host, port, admin creds lub pre-created database)
  - Po wyborze: pokazuje plan "utworzymy database X, role Y, uruchomimy 3 migracje"

## §20.3 Status API gaps (po §20)

API-3 (SQL API) i API-4 (manifest [database]) z §19.2 zostają, ale finalne:

| # | API | Notatka |
|---|-----|---------|
| API-3 | `sql_exec/query/transaction` host functions | **F1**, działa z SQLite i PostgreSQL przez core proxy |
| API-4 | `[storage]` manifest sekcja (kv/sql/sql_backends/sql_dialect/migrations_dir) | **F1** |
| API-4a | Migrations runner (uporządkowany apply przy install/upgrade) | **F1** |
| API-4b | Per-addon FS sandbox (`~/.tentaflow/addons/<id>/`) | **F1**, infrastructure dla SQLite plus przyszłe FS |
| API-4c | PostgreSQL connection manager + per-addon role lifecycle | F2 (MVP można puścić tylko SQLite) |

Pełny RFC z signaturami ABI w `tentavision-addon-api-gaps.md` (do utworzenia).

## §20.4 Następne kroki

1. ✅ §20 finalne storage
2. ⏳ Mockup M14 (przepisany — alias mapping AI + storage stats KV/SQL/Vector/Recording)
3. ⏳ Mockup M15 (install wizard — z krokami: permissions, storage backend, aliasy AI, flow templates, network rules, profil prawny, pierwsza kamera)
4. ⏳ Osobny RFC `tentavision-addon-api-gaps.md` z pełnymi ABI

---

# §21. v0.3.4 — korekta modelu aliasów (po sprawdzeniu kodu)

User wskazał że nie zrozumiałem aliasów. Sprawdziłem w kodzie i potwierdzam: model jest **istotnie inny** niż w §19/§20.

## §21.1 Faktyczny model aliasów (z kodu)

`tentaflow-core/src/db/migrations.rs:225`:
```sql
CREATE TABLE model_aliases (
    id INTEGER PRIMARY KEY,
    alias TEXT UNIQUE NOT NULL,
    target_model TEXT NOT NULL,
    is_active INTEGER DEFAULT 1,
    fallback_targets TEXT,          -- lista fallback target-ów
    strategy TEXT DEFAULT 'first_available'
);
```

`tentaflow-core/src/addon/mod.rs:1890` (teams-bot) — addon przy aktywacji wywołuje:
```rust
crate::db::repository::create_or_reactivate_model_alias(
    &self.db, alias, default_target, "first_available"
);
```
Przy dezaktywacji: `set_model_alias_active(db, alias, false)`. Lista aliasów twardo zdefiniowana jako `TEAMS_BOT_ALIASES` (5 sztuk dla teams-bot).

Plus jest druga tabela `service_aliases` (alias → service_id 1:1, prostszy mechanizm).

## §21.2 Co robi addon, co robi admin

| Aspekt | Addon | Admin TentaFlow |
|--------|-------|-----------------|
| Tworzenie aliasu | ✅ przy aktywacji (`create_or_reactivate_model_alias`) | ✅ ręcznie w UI |
| Deklaracja sugerowanego default w manifeście | ✅ pole `suggested_default` | — |
| Konfiguracja `target_model` | ❌ tylko podpowiedź | ✅ wymagana decyzja |
| Konfiguracja `fallback_targets` | ❌ | ✅ wymagana decyzja (chain) |
| Konfiguracja `strategy` | ❌ | ✅ wymagana decyzja (`first_available` / `round_robin` / `weighted`) |
| `is_active` | ✅ ON przy aktywacji, OFF przy dezaktywacji | ✅ override |
| Wołanie aliasu | ✅ `service_call(alias, method, payload)` | — |
| Odczyt aktualnego target (readonly) | ✅ jakim modelem rozwiązuje | ✅ |
| Odczyt który target faktycznie wykonał (po fallbackach) | ✅ w response metadata | ✅ |
| Usunięcie aliasu | ❌ (tylko dezaktywacja) | ✅ ręcznie |

## §21.3 Wynikowe API SDK

```rust
// SDK — alias management (addon-side)
alias_create(spec: AliasSpec) -> Result<()>
//   spec.id, spec.suggested_default, spec.methods, spec.gate
alias_deactivate(alias_id: &str) -> Result<()>
alias_get(alias_id: &str) -> Result<AliasInfo>     // readonly: target, fallbacks, strategy, last_used_target
alias_list_owned() -> Result<Vec<AliasInfo>>       // tylko aliasy utworzone przez ten addon

// SDK — wywołanie (główne API)
service_call(alias: &str, method: &str, payload: &[u8]) -> Result<ServiceResponse>
struct ServiceResponse {
    payload: Vec<u8>,
    executed_by: String,      // który konkretny target wykonał (po fallbackach)
    duration_ms: u32,
    fallback_used: bool,
}
```

Manifest deklaracja aliasów (sekcja `[[alias]]`, propozycyjna):
```toml
[[alias]]
id = "tentavision-yolo"
display_name = "Detektor obiektów (D1, D6)"
methods = ["detect", "track"]
suggested_default = "yolo11m-detector"   # może być pusty
gate = ""                                # opcjonalnie nazwa gate'a

[[alias]]
id = "tentavision-face-embed"
display_name = "Face embedding (D4)"
methods = ["embed"]
suggested_default = ""
gate = "d4-historical"                   # alias tworzony z is_active=0 dopóki gate niezaspokojony
```

Po instalacji addon wywołuje `alias_create` dla każdego — TentaFlow zapisuje do `model_aliases` z `target_model = suggested_default` (lub pusty) i `is_active = !gate_required`.

## §21.4 Konsekwencje dla mockupów (wykonane)

- **M14 → readonly view aliasów** (wykonane):
  - Tabela 6 aliasów TentaVision z: alias name + methods | current target (z `model_aliases`) | fallback chain | strategy | last used target + count | active status
  - Link out: "Skonfiguruj w Serwisy → Aliasy (M16)"
  - Sekcje storage (KV / SQL / Vector / Recording / Camera / Streaming / Evidence) zostają

- **M15 krok 3 → deklaracja aliasów** (wykonane):
  - Lista 6 aliasów z manifestu z `suggested_default`
  - Status per alias: "will be created (with suggested target)" / "will be created (empty target)" / "created inactive (gated)" / "already exists"
  - **Bez** dropdown wyboru konkretnego targetu
  - Wskazanie po instalacji: "Przejdź do Serwisy → Aliasy aby przypisać konkretne modele"

- **M16 (nowy) — globalny UI Aliasy** (wykonane):
  - Sidebar TentaFlow z aktywnym **Services**
  - Tabs w main: Wszystkie serwisy / Modele / **Aliasy (aktywny)** / Węzły / Historia
  - Lista wszystkich 21 aliasów (6 TentaVision + 5 teams-bot + 3 manual + reszta), per alias: name, owner (addon/manual), target+fallback chain, strategy, last used target, active toggle, edit button
  - Inline edit dialog dla tentavision-yolo: dropdown primary target, strategy radio (first_available/round_robin/weighted), fallback builder z drag-to-reorder, metadata (owner, created, last change, użycie 24h z fallback stats)
  - Filtry: po owner, active, strategy, fallbacks, empty target

## §21.5 Co znika z planu

- Sekcja §19 mówiła o "binding capability resolver" — to było codex over-engineering. **Wyrzucam**, aliasy są proste: addon woła alias, admin konfiguruje cel.
- M14 capability bindings explorer (poprzednia wersja) — wyrzucone, zastąpione readonly view.
- M15 krok mapowania target → alias — wyrzucony, zastąpiony deklaracją.

## §21.6 Co dodaje §21 do API gaps

| # | API | Priorytet | Notatka |
|---|-----|-----------|---------|
| API-1' (zmiana) | `service_call(alias, method, payload) -> ServiceResponse{executed_by, duration_ms, fallback_used}` | F1 | obecnie response nie ma metadata kto wykonał |
| API-1a (nowe) | `alias_create / alias_deactivate / alias_get / alias_list_owned` host functions | F1 | dziś addon mod.rs wywołuje wewnętrznie repository — trzeba wystawić do WASM ABI |
| API-1b (nowe) | `[[alias]]` w manifeście (`id`, `methods`, `suggested_default`, `gate`) | F1 | dziś teams-bot ma hard-coded listę w mod.rs — trzeba przenieść do manifestu |
| API-1c (nowe) | M16 — UI **Services → Aliasy** w globalnym TentaFlow www | F1 | dziś brak UI (grep nie znalazł), CLI/SQL only |

`service_aliases` (1:1) zostaje jako prostszy mechanizm dla aliasów do konkretnych serwisów (np. `meeting-bot-sidecar` → service_id). Dla TentaVision używamy `model_aliases` (z fallback chain).

## §21.7 Status mockupów po §21

| ID | Status | Notatka |
|----|--------|---------|
| M1–M3, M5–M13 | bez zmian | nadal aktualne, używają aliasów przez `service_call` |
| M4 v2 | bez zmian | profile + Flow selection + quick params |
| M14 v2 | **przepisany** | readonly aliases + storage stats |
| M15 v2 | **krok 3 przepisany** | deklaracja aliases zamiast mapowania |
| M16 | **nowy** | globalny UI TentaFlow → Services → Aliasy |

Index zaktualizowany: 16 mockupów (15 TentaVision + 1 systemowy).
