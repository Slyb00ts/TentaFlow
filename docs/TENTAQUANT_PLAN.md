# TentaQuant — plan wdrożenia

> **Rewizja 0.1** (2026-09-03). Wbudowana aplikacja natywna Core (NIE addon WASM), na platformie
> aplikacji (`addon_packages` + instancje), na wzór TentaNas i Code Studio — z jedną różnicą, której
> żadna z dotychczasowych aplikacji nie potrzebowała: **wiele instancji jednego pakietu**, każda
> z własną bazą, członkami, dostawcami QPU i limitami. Instancja to *laboratorium*: grupa
> studencka, zespół badawczy, warsztat firmowy.
>
> Produkt: laboratorium obliczeń kwantowych — pisanie kodu (Python + OpenQASM 3), wizualna edycja
> obwodów z natychmiastowym podglądem stanu, uruchamianie na pięciu warstwach (przeglądarka, Core,
> Python CPU, GPU, prawdziwe QPU) z tego samego artefaktu, programy hybrydowe CPU+GPU+QPU,
> katalog przykładów w trzech wariantach i ścieżka nauki z automatyczną oceną.
>
> Odniesienia `plik:linia` pochodzą z audytu repo z 2026-09-03; fakty o ekosystemie (wersje,
> licencje, cenniki) z rozpoznania w sieci z tego samego dnia — źródła w §1. Pozycje oznaczone
> **[niezweryfikowane]** wymagają potwierdzenia przed decyzją.
>
> Uprawnienia: plan opiera się na **obecnym** modelu platformy aplikacji — katalog uprawnień
> w `[[permission]]` manifestu, matryca per instancja administrowana w Addons (zakładki
> Uprawnienia i Widoczność), hierarchia `PermissionChecker` — a nie na migracjach ról org, które
> opisuje starszy `CODE_STUDIO_PLAN.md` (§2.1, §10). TentaQuant nie zakłada ani jednej tabeli
> członkostwa własnej: członkostwo w laboratorium *jest* wpisem w matrycy jego instancji.

---

## 0. Decyzje bazowe

| Obszar | Decyzja |
|---|---|
| Osadzenie | Aplikacja natywna na platformie aplikacji (`NATIVE_APP_PACKAGES` + `NativeAppHooks`), `singleton = false` — **pierwsza aplikacja wieloinstancyjna**; luki platformy w §2.2 zamykamy w Fazie 0 |
| Instancja | = laboratorium: własna `tentaquant.db`, własna matryca uprawnień (= członkostwo i role), własne opcjonalne konto laboratorium IBM z pulą sekund QPU. Kafelek na apps-home per instancja |
| Uprawnienia | Wyłącznie platforma aplikacji: `[[permission]]` w manifeście → matryca per instancja (`addon_permissions` per grupa / per użytkownik / default) edytowana w **Addons → instancja → Uprawnienia**, widoczność kafelka w **Widoczność**. Żadnej własnej tabeli członków, żadnej migracji ról org, żadnego własnego ekranu „członkowie” — role laboratorium to zestawy uprawnień z manifestu |
| Artefakt kanoniczny | **OpenQASM 3** dla obwodów; **Python** dla programów; oba trzymane w notatniku (komórki) i wersjonowane. Nie budujemy własnego języka |
| Język użytkownika | **Python + Qiskit** (domyślnie) i **OpenQASM 3** (obwody). Obok, w tym samym jądrze: CUDA-Q i PennyLane. Nikt nie pisze w Ruście — nasz symulator jest silnikiem, nie językiem |
| Symulator | **Własny crate `tentaflow-quantum`** (Rust): parser OQ3 (crate'y Qiskita `oq3_*`), IR obwodu, symulator wektora stanu (CPU + **GPU przez wgpu**: Vulkan na NVIDIA/AMD/Intel, Metal na Apple, WebGPU w przeglądarce) + stabilizatorowy. Kompilowany **trzy razy**: natywnie do Core, do `wasm32` dla przeglądarki i jako moduł Pythona (pyo3/maturin) do jądra. Jeden kod, jedne wyniki, testy złote wobec Qiskit Aer. Precyzja GPU = `complex64` (WGSL nie ma f64); `complex128` tylko na CPU |
| GPU — jednolicie | Użytkownik pisze `backend = tentaquant.backend("gpu")` i to samo działa na każdym GPU, bo `BackendV2` Qiskita opakowuje **nasz** symulator: backend `cuda` na NVIDIA, `wgpu` (Vulkan) na AMD/Intel, `wgpu` (Metal) na Apple — jeden kod kerneli, trzy backendy, jedne wyniki (§6.3). cuStateVec, CUDA-Q i Aer-GPU **nie są** targetami TentaQuant w v1: użytkownik może je doinstalować we własnej sesji jądra, ale to jego kod, nie warstwa T3. Zgodne z konwencją repo: AMD/Intel na Vulkanie, brak HIP/ROCm |
| Klasyczny kod na GPU | Trzecia oś porównania: ten sam problem klasycznie na CPU (NumPy), klasycznie na GPU (CuPy/Numba/PyTorch, w tym samym jądrze) i kwantowo. Widok porównania mierzy czas i jakość po rozmiarze problemu |
| Warstwy wykonania | T0 przeglądarka (WASM + WebGPU, natychmiast; komórki Python przez Pyodide, z fallbackiem „jądro liczy, przeglądarka wykonuje” — §4.6), T1 Core (natywny symulator, CPU i wgpu), T2 `quantum-python` (Qiskit/Aer/PennyLane CPU + nasz moduł `tentaquant_sim`), T3 GPU = **ten sam `tentaflow-quantum` na GPU wybranego węzła** (`cuda` / `wgpu`-Vulkan / `wgpu`-Metal), w procesie Core albo w jądrze przez wheel pyo3 — bez osobnej usługi Python, T4 QPU (IBM). Ten sam obwód/program uruchamia się na każdej przez zmianę **targetu**, nie kodu |
| Dystrybucja | Usługi TentaQuant to **python-bundle** wbudowane w binarkę (jak `test-runner`): tar.gz w `include_bytes!`, Python i wheel-e pobiera uv przy pierwszym deployu. **Instalacja instancji w Addons uruchamia deploy usługi automatycznie** (zadanie w tle z postępem) — luka platformy do zamknięcia, §2.5 |
| Wykonanie Pythona | Sesje jądra Jupyter (`ipykernel`) **wewnątrz** usługi `quantum-python`; usługa mówi do Core protokołem komunikatów Jupyter (podzbiór iopub) po HTTP/SSE; dashboard nigdy nie widzi Jupytera — dostaje strumień przez protokół binarny |
| Kod z sandboxa do Core | Wyłącznie kanał zwrotny usługi (`reverse_requests = true`, jak `teams-bot`): SDK `tentaquant` w jądrze → broker w usłudze → strumień zwrotny → Core. Sandbox **nigdy** nie trzyma poświadczeń dostawcy |
| Broker QPU | W Core. Trait `QpuProvider` o kształcie kontraktu QRMI (`acquire/release/task_start/status/result/logs/target`). Dostawca v1: **tylko IBM** (Qiskit Runtime REST natywnie w Rust), z konta osobistego użytkownika albo z konta laboratorium. Trait zostaje abstrakcyjny (jeden implementator); IonQ, IQM, QRMI — §7.4, v2 |
| Hybryda | Poziom **zadania** (ms–s), nie czasu rzeczywistego. Program hybrydowy = kod Python, w którym `tq.run(..., target=...)` kieruje etapy na CPU/GPU/QPU; pipeline hybrydowy = flow z blokami `quantum_run` / `quantum_program` i wyborem węzła. Sprzężenie zwrotne w µs (NVQLink) jest poza zasięgiem i **nie jest obiecywane** |
| Węzeł | Użytkownik wybiera węzeł dla T2/T3 (jak ML Studio); UI pokazuje GPU węzła z heartbeatu. `device="auto"` to **prosta reguła** (§5.3: liczba kubitów + dostępność, nigdy QPU), bez kolejki i schedulera w v1 |
| Izolacja | `container` domyślnie, gdy węzeł ma runtime kontenerów (kod użytkowników jest niezaufany); `trusted_native` (python-bundle bez kontenera) **tylko** gdy matryca instancji daje `quant.run` jednej osobie albo `quant.admin` potwierdzi jawnie z ostrzeżeniem; laboratorium wieloosobowe bez runtime kontenerów nie dostaje T2 (zostaje T0/T1/T3-Core). Ten sam rozkład gwarancji co Code Studio §7.1, ale odwrócony domyślny |
| Poświadczenia QPU | **Domyślnie osobiste**: każdy użytkownik podaje własny token IBM (API key + CRN) w swoich ustawieniach laboratorium; widzi go tylko on, run idzie z jego konta, bez zgody i bez puli. Obok, opcjonalnie, **jedno konto laboratorium** per instancja (`quant.admin`) dla osób bez własnego tokenu — z pulą sekund QPU na okres i limitem per osoba. Oba zaszyfrowane `SettingsCipher`, odszyfrowane w jednym miejscu (submit), nigdy w logach, nigdy w sandboxie |
| Zgody | Kosztorys przed **każdym** wysłaniem na QPU. Z konta osobistego: potwierdzenie użytkownika i koniec. Z konta laboratorium: pula sekund instancji + limit per osoba + zgoda opiekuna (`quant.instruct`), widoczna w karcie „Do zatwierdzenia” na Pulpicie i w powiadomieniach Core. `quant.admin` zawsze rola Admin organizacji |
| Przykłady | Katalog wbudowany w binarkę (`include_dir`), każdy przykład w wariantach `cpu`/`gpu`/`qpu` + obwód OQ3 + README; instancja kopiuje je do siebie tylko przy „forku” |
| Kurs | 24 zadania (katy) w jednej kolejności od najprostszego do najtrudniejszego, **bez terminów i bez przydziałów** (to nie jest LMS — adresat to każdy użytkownik: firma, uczelnia, hobbysta); ocena **w Core** natywnym symulatorem (deterministycznie, bez Pythona); postęp, punkty, seria i ranking per użytkownik w instancji, ranking wyłączalny przez opiekuna w Ustawieniach |
| UI | Notatnik (komórki kod/markdown/obwód), Studio obwodów (drag-drop + żywy stan), Runy, Urządzenia, Przykłady, Kurs, Ustawienia. Nowe komponenty: `tf-quantum-circuit`, `tf-bloch-sphere`, `tf-mime-output` |
| Mesh | Żądania przez `AppRouteOp`; strumienie sesji jądra przez **uogólniony** relay Code Studio (`mesh_stream.rs`) — §11.3 |
| Poza zakresem | Własny język/kompilator, Q#, pulsy/defcal, symulacja szumu poza kanałami Krausa Core (T1) i modelem Aer (T2), real-time QEC, marketplace dostawców, IonQ/IQM/QRMI w v1, osobna usługa GPU (cuQuantum/CUDA-Q), terminy/przydziały/oceny (LMS) |

---

## 1. Rozpoznanie ekosystemu (stan na 2026-09-03)

### 1.1 QRMI — Quantum Resource Management Interface

- Co to jest: cienka, niezależna od dostawcy warstwa dostępu/kontroli/monitoringu komputerów
  kwantowych, projektowana pod menedżery zasobów HPC. Repo `qiskit-community/qrmi`, dokumentacja
  IBM (`quantum.cloud.ibm.com/docs/en/guides/qrmi`, `.../slurm-plugin`), artykuły arXiv:2506.10052
  (2025-06) i arXiv:2607.19591 (2026-07, 39 autorów: IBM, LLNL, ORNL, Pasqal, NQCC).
- Język: **Rust**; bindingi Python (pyo3, abi3, ≥3.11), C (cbindgen), Lua. Licencja **Apache-2.0**
  (od 0.24.1). Najnowsze wydanie **0.24.2 (2026-08-29)**; wydania niemal co tydzień; 0.24.0
  przepisało `QRMIService` i zastąpiło `anyhow` typowanym `QrmiError`, 0.23.0 zmieniło nazwę
  usługi IBM. **Pre-1.0, API zmieniane dwukrotnie w sierpniu.**
- Backendy (`ResourceType`): `ibm-quantum-system` (IBM Direct Access — tylko plan On-Prem),
  `ibm-quantum-compute-service` (IBM Cloud), `pasqal-cloud`, `pasqal-local`, `alice-bob-felis`,
  `iqm-server`. Konfiguracja w całości przez zmienne środowiskowe.
- Kontrakt: trait `QuantumResource` — `acquire() -> token`, `release(token)`, `task_start(Payload)
  -> task_id`, `task_stop`, `task_status` (Queued/Running/Completed/Failed/Cancelled),
  `task_result`, `task_logs`, `target()`, `metadata()`, `is_accessible`. `Payload`:
  `QiskitPrimitive{input, program_id}`, `PasqalCloud`, `AliceBobFelis{human_qir}`, `IQMServer`.
- **Nie ma na crates.io** — tylko zależność `git`. Slurm: `spank-plugins` 0.11.0 (GPL-3.0,
  `#SBATCH --qpu=...`). Produkcyjnie potwierdzone w CINECA (Pasqal SOL, 140 q). **`qrun` nie
  istnieje** — pojawia się tylko jako niezdefiniowana opcja w `docs/ux.md`.
- Wniosek: QRMI to właściwy **kształt** kontraktu brokera (i most do HPC/EuroHPC), ale zbyt
  niestabilny, by być jedyną implementacją. §7.

### 1.2 IBM Quantum i Qiskit

- `qiskit` **2.5.2** (2026-08-13, Apache-2.0, Python 3.10–3.14). Crate'y Rust Qiskita
  (`crates/{circuit,transpiler,qasm3,...}`) **nie są publikowane** na crates.io; C API jest
  „eksperymentalne”; `qiskit-rs` (binding Rust nad C API) tylko z gita, ~15 gwiazdek. **Nie
  budujemy na tym.**
- `qiskit-ibm-runtime` **0.49.0** (2026-08-10): tylko `SamplerV2`/`EstimatorV2` (+ beta
  `Executor`); tryby job / batch / session — **plan Open nie może używać sesji**. Kanał
  `ibm_quantum` usunięty w 0.41 (2025-07).
- REST: `https://quantum.cloud.ibm.com/api/v1/` (EU: `eu-de.`), nagłówki `Authorization: Bearer
  <IAM>` (≤1 h), `Service-CRN`, `IBM-API-Version`; `/backends`, `/jobs` (wejście **OpenQASM 3**,
  ISA-transpilowany), `/sessions`; OpenAPI publiczne. To jest ścieżka dla brokera w Rust.
- Plany: **Open = darmowy, 10 min na 28-dniowe okno kroczące, tylko us-east**; Flex ≥400 min
  ($72/min), PAYG ($96/min), Premium ($48/min). Od 2026-04 Heron r2 `ibm_kingston` (156 q) dla
  Open. Dostępność Heron r3/Nighthawk dla Open **[niezweryfikowane]**.
- Obwody dynamiczne: na wszystkich backendach, ale tylko `if_test` (bez pętli, bez zagnieżdżeń,
  tylko Sampler). Testy lokalne: `fake_provider` (`FakeKingston`, `FakeAachen`) lub Aer.
- `qiskit-aer` 0.17.2 (CPU); `qiskit-aer-gpu` na PyPI **utknął na 0.15.1 / CUDA 12
  (2024-09)** — GPU trzeba budować ze źródeł (`AER_THRUST_BACKEND=CUDA`).
- **Qiskit Code Assistant (hostowany) wyłączony 2026-05-29**; modele otwarte na HF
  (`granite-3.3-8b-qiskit`, `Qwen2.5-Coder-14B-Qiskit`, GGUF). Serwujemy je istniejącym
  llama.cpp/vLLM jako zwykły alias — §12.4.
- `qiskit-ibm-transpiler` 0.18.0: lokalne passy AI (routing, synteza Clifford) bez planu Premium.

### 1.3 Frameworki hybrydowe CPU/GPU/QPU

- **NVIDIA CUDA-Q** `cudaq` **0.15.1** (2026-08, Apache-2.0): `@cudaq.kernel`, `sample/observe/
  run/vqe`, MLIR (`quake`) → QIR; targety symulacyjne `qpp-cpu`, `nvidia` (fp32/fp64),
  `nvidia-mgpu` (MPI), `nvidia-mqpu`, `tensornet`, `tensornet-mps`, `density-matrix-cpu`, `stim`;
  targety sprzętowe IonQ, Quantinuum (natywny MCM + branching), IQM, OQC, Pasqal (przez QRMI od
  0.14), QuEra, Braket, qBraid, Scaleway… **IBM nie jest targetem** (most `cudaq-ibm-anton` przez
  OQ2, 2026-09-02). Import z Qiskit/OpenQASM w 0.15 — nazwa funkcji **[niezweryfikowana]**.
  Kontenery `nvcr.io/nvidia/quantum/cuda-quantum`; Linux x86_64/ARM64, CUDA 12/13; macOS tylko CPU.
- **NVQLink** / `cudaq-realtime` (0.14, osobny instalator C++): pętla FPGA→GPU→FPGA <4 µs — to
  jest „prawdziwa” hybryda czasu rzeczywistego i wymaga sprzętu, którego nie mamy. **Nasza hybryda
  jest na poziomie zadań.**
- **cuQuantum 26.6.0** (cuStateVec, cuTensorNet, cuDensityMat, cuStabilizer); integracje z Aer,
  PennyLane `lightning.gpu`, QuEST, TKET, CUDA-Q.
- **PennyLane 0.45.1** + Catalyst 0.15 (`@qjit`, MLIR, pętle i gradienty w skompilowanym
  programie); `lightning-gpu/-kokkos/-amdgpu`. Najlepszy model „programu wariacyjnego” do nauki.
- Kształt programu hybrydowego w praktyce: pętla optymalizatora po stronie klasycznej
  (`EstimatorV2.run` w batch/session u IBM, `cudaq.vqe` na `nvidia-mqpu`, QNode w autodiff
  u PennyLane). To dokładnie kształt, który SDK `tentaquant` ma odtworzyć niezależnie od targetu (§5).
- Braket Hybrid Jobs (kontener z priorytetową kolejką QPU, $0.30/task + shoty), Azure Quantum
  (QIR; IonQ, Pasqal, Quantinuum, Rigetti) — dostawcy drugiej fali, §7.4.

### 1.4 Reprezentacje pośrednie

- **OpenQASM 3**: spec 3.1.0 (2024-05), TSC IBM/Microsoft/ZI/AWS. Parsery Rust Qiskita
  `oq3_lexer/oq3_parser/oq3_syntax/oq3_semantics` **0.7.0 (2024-10)**, Apache-2.0, ~880k pobrań,
  repo żywe, ale bez wydania od ~22 miesięcy; to ich używa `qiskit.qasm3.loads_experimental`.
  Innego parsera OQ3 w Rust nie ma. Wymaga potwierdzenia budowy pod `wasm32` — spike F0.
- Co przyjmują targety: IBM tylko OQ3 (podzbiór: `if/else`, `delay`, arytmetyka; bez `for/while/
  def/box`); Braket własny dialekt OQ3 z pragmami; IonQ podzbiór `ionq.qasm3.v1` bez rozgałęzień;
  Quantinuum **QIR lub OQ2**.
- **QIR** (LLVM, profile Base / Adaptive): uniwersalna ścieżka sprzętowa Azure/Quantinuum/Rigetti,
  emitowana przez CUDA-Q. `pyqir` 0.12.5 (MIT), `qbraid-qir` 0.6.1 (OQ3→QIR), `qir-runner` 0.9.6
  (Rust, MIT, nie na crates.io). `qiskit-qir` Microsoftu zarchiwizowane.
- Wniosek: **OQ3 jako artefakt kanoniczny edytora**; adaptery dialektów (IBM/IonQ/Braket) w Core
  lub w usłudze; QIR emitowane w usłudze Python (`qbraid-qir`/`pyqir`) dla Quantinuum/Azure.

### 1.5 Symulatory

| Kandydat | Wersja / licencja | Ocena |
|---|---|---|
| Microsoft QDK `qdk_simulators` (Rust; full-state, sparse, stabilizer, noise, `gpu` = wgpu) | 1.31.0 (2026-07), MIT; **nie na crates.io** | Najlepiej przetestowany Rust; ale git-only i sprzężony z workspace Q# |
| npm `qsharp-lang` (Rust→WASM kompilator+symulator w workerze, `ux`: `Circuit` edytowalny, `BlochSphere`, `Histogram`) | 1.31.0, MIT | Jedyna gotowa „bardzo interaktywna” warstwa w przeglądarce — ale UI w Preact, duży bundle, model Q#; koliduje z regułą `tf-*` |
| `qip` (RustQIP) | 1.5.0 (2025-12), MIT | Czysty Rust, małe zależności; wasm nie deklarowany **[niezweryfikowane]** |
| `prism-q` | 0.31.0 (2026-09), MIT/Apache | OQ3 in/out, AVX2, opcjonalnie CUDA+MPI; młody |
| `roqoqo` (HQS) | 1.22.2, Apache-2.0 | Toolkit obwodów bez symulatora; `roqoqo-quest` natywnie |
| `quantrs2-*` | 0.2.1 (2026-08) | 11 crate'ów, wgpu; ciężkie zależności, 16 gwiazdek |
| `stim` (stim-rs, cxx) | 0.4.5, Apache-2.0 | Clifford na dużą skalę, natywnie |
| `qiskit-aer`, Qulacs 0.6.14, QuEST 4.2.0, quimb 1.15 | Python/C++ | Warstwa T2/T3 |
| `quantum-circuit` (JS, quantastica) | 0.9.250 (2026-08), MIT | Bez UI; macierz eksportu (Qiskit, Cirq, CudaQ, Braket, Q#, Quirk, SVG) |
| Quirk | 2019, Apache-2.0 | Tylko jako iframe „brudnopis”, nie zależność |
| Pyodide 314.0.6 (2026-08-25, CPython 3.14) | MPL-2.0 | numpy/scipy/matplotlib/networkx/sympy w dystrybucji; `rustworkx` ≥ 0.17 buduje się pod Pyodide, a od 0.18.1 publikuje eksperymentalne kółko `pyemscripten_2026_0_wasm32` na PyPI (PEP 783); **Qiskit nie ma kółka wasm** — PR #15484 „[WIP] WASM build for qiskit-cext” to otwarty szkic (2025-12 → 2026-05, tylko `getrandom`, nietestowany). Zależności Qiskita (main): numpy, scipy, rustworkx, dill, stevedore, typing-extensions — poza `qiskit._accelerate` (pyo3) wszystko już jest w Pyodide albo jest czystym Pythonem. Ścieżka T0 dla Pythona: §4.6 |

Wniosek: żaden gotowy element nie spełnia naraz: Rust, wasm, brak obcego UI, ten sam kod
w Core i w przeglądarce. Stąd własny crate (§6), rozmiaru porównywalnego z `tentaflow-voxel-wasm`,
z `oq3_*` jako jedyną istotną zależnością. Jako punkt odniesienia liczbowego (testy złote) — Aer.

#### 1.5.1 Silniki symulacji × producent GPU

Wymaganie: to samo od strony Pythona na NVIDIA, AMD, Intel i Apple. Żaden istniejący silnik tego
nie daje:

| Silnik | NVIDIA | AMD | Intel GPU | Apple GPU | Przeglądarka |
|---|---|---|---|---|---|
| qiskit-aer 0.17.2 | CUDA + cuStateVec (wheel `cu11`; wheel CUDA 12 stoi na 0.15.1) | ROCm tylko ze źródeł, bez CI | — | — | — |
| PennyLane Lightning 0.45 | `lightning.gpu` (wheel) | `lightning.amdgpu` (MI300, ROCm ≥ 7) | Kokkos-SYCL nietestowany | tylko CPU | — |
| Symulatory na PyTorch 2.14 | CUDA | ROCm | XPU (complex **[niezweryfikowane]**) | MPS `complex64` z lukami, bez `complex128` | — |
| Symulatory na JAX 0.11 | CUDA | ROCm | wtyczka rok w tyle | `jax-metal` porzucony | — |
| Qrack 10.12 (OpenCL) | ✓ | ✓ | ✓ | tylko CPU (brak Metal) | — |
| QuEST 4.2 | CUDA/cuQuantum | HIP | — | — | — |
| CUDA-Q 0.15 / cuQuantum 26.06 | ✓ | — | — | — | — |
| **wgpu (własny)** | Vulkan | Vulkan | Vulkan | Metal | WebGPU (Chrome, Safari 26; Firefox na Linuksie za flagą) |

Ograniczenia wgpu, które trzeba zaprojektować, nie odkryć: WGSL liczy w f32 (`complex64` jako
`vec2<f32>`; f64 tylko przez `SHADER_F64` na Vulkanie, 16–64× wolniej — nie używamy), jeden
bufor storage ≤ 2 GiB na Vulkan/DX12 (Metal: `maxBufferLength`, 8 GB na M1), więc stan > 2^28
amplitud jest **shardowany** po ≤ 8 buforach (`binding_array`, przykład `big_compute_buffers`
w wgpu, natywnie), a powyżej VRAM — blokowo wymieniany z RAM (wzór `kbw`). Precedens „Rust + wgpu
+ pyo3” istnieje (quantrs2), ale jest cienki. W repo wgpu już jest (`tentaflow-voxel-wasm`
29.0.3, Burn-wgpu dla wizji), a konwencja „AMD i Intel na Vulkanie, bez HIP” obowiązuje
(`tentaflow-core/Cargo.toml:49`). Na NVIDIA szybsze pozostaną cuStateVec/CUDA-Q — dlatego
interfejs jest jeden, a implementacja pod spodem wybierana per węzeł.

### 1.6 Dostęp do sprzętu

| Dostawca | Dostęp | Darmowy próg | Uwagi |
|---|---|---|---|
| **IBM** | REST + `qiskit-ibm-runtime` | Open: 10 min/28 dni, `ibm_kingston` 156 q | Najistotniejszy dla początkujących; EU `eu-de` tylko płatne |
| **IQM Resonance** | REST, `iqm-client` 35.0.2, `IQM_TOKEN` | **Starter: 30 kredytów/mies.** | Najlepszy darmowy dostęp do prawdziwego QPU; jest w QRMI |
| **IonQ** | REST `api.ionq.co/v0.4`, `Authorization: apiKey` | symulator darmowy | Prosty REST → implementacja w Rust |
| Quantinuum Nexus | `qnexus`, logowanie device-code | emulatory | QIR/OQ2; kwoty HQC |
| Rigetti QCS | `pyquil` 4 (`qcs-sdk` Rust) | brak | przez Braket/Azure |
| Pasqal | `pasqal-cloud` + Pulser | emulator `EMU_FREE`; 100 h Orion dla EU R&D (wniosek) | jest w QRMI; analogowy model |
| Amazon Braket | SDK 1.127 | 1 h sym./mies. przez 12 mies. | $0.30/task uniwersalnie |
| Azure Quantum | `azure-quantum` 3.12 | kredyty **[niezweryfikowane]** | QIR |
| qBraid (agregator) | SDK + `qbraid-qir` | $50 kredytów co 90 dni | 20+ urządzeń |
| **PIAST-Q** (PSNC Poznań, AQT 20 q) | brak publicznego API | early access od 2026-04-14, formularz tylko dla PL | konsorcjum PSNC + CFT PAN + Creotech; QRMI/Slurm **[niezweryfikowane]** |
| EuroHPC „Quantum Access Pilot” | wniosek, cut-off miesięczny (od 2026-06-25) | darmowy | JADE, Ruby/Lucy, PIAST-Q, VLQ, Euro-Q-Exa, SOL |

### 1.7 Edukacja — co mają istniejące platformy

IBM Quantum Learning (14 kursów z egzaminem, Composer z OQ3 od 2026-08), Quantum Katas
Microsoftu (MIT; format `Placeholder.qs` + `Verification.qs` + `Solution.qs`, ocena w przeglądarce,
od 2026-06 przenoszone do VS Code), Quirk (drag-drop ≤16 q, Chance/Bloch/Amplitude), Black Opal
(250+ aktywności, sfera Blocha, odznaki, AI-tutor od 2026-04), PennyLane Codebook (ćwiczenia
z autosprawdzaniem bez konta), QWorld (Jupyter), Brilliant. Wspólny mianownik: kod w przeglądarce
z automatycznym sprawdzeniem, sfera Blocha, histogram/amplitudy, notatnik, postęp, w 2026 asystent
AI. RCT (arXiv:2507.21721, n=146): sfera Blocha nie poprawia wyniku nauki, ale skraca czas zadań.

### 1.8 Środowiska wykonania

- Protokół jąder Jupyter 5.5: `execute_request` → iopub `status:busy` → `stream` / `display_data`
  / `update_display_data` (po `display_id`) / `execute_result` / `error` → `execute_reply` →
  `status:idle`. `ipykernel` 7.3.0 (BSD-3). Klienty Rust (runtimed, BSD-3): `jupyter-protocol`
  2.0.2, `jupyter-zmq-client` 1.0.1 — ale ZMQ przez granicę kontenera to zbędna komplikacja; jądro
  zostaje **w usłudze**, usługa wystawia HTTP/SSE. Marimo 0.24 (ASGI, reaktywne) — rozważone,
  odrzucone: własny frontend i model reaktywny nie pasują do komórek sterowanych przez Core.
- Sandbox: ten sam kształt co `SandboxLimits::test_runner` (rootfs RO, tmpfs, `cap_drop ALL`,
  limity) + `--gpus` dla T3; gVisor `--nvproxy` jako opcja twardsza, nie wymóg.

### 1.9 Co gdzie — synteza

| Warstwa | Co | Skąd |
|---|---|---|
| Rust Core + WASM | parser OQ3, IR, symulator SV/stabilizer (CPU + `cuda` + `wgpu`), kanały Krausa, ocena kat, broker QPU (IBM REST), pula sekund, audyt | `tentaflow-quantum`, `oq3_*` |
| Usługa `quantum-python` | Qiskit 2.5 + runtime 0.49 + Aer 0.17 + PennyLane 0.45 + `qdk` 1.31 + `cirq` + `qbraid-qir`/`pyqir`; jądra ipykernel; transpile do ISA; SDK `tentaquant` | python-bundle + Docker |
| Przeglądarka | WASM z tego samego crate'a; `tf-quantum-circuit`, `tf-bloch-sphere`, `tf-mime-output` | własne |
| Chmury | IBM (v1); IonQ/IQM/Braket/Azure/Quantinuum/qBraid (v2) | broker Core |

---

## 2. Stan faktyczny repo

### 2.1 Platforma aplikacji natywnych — co dostajemy za darmo

- Rejestracja pakietu: `NATIVE_APP_PACKAGES` (`tentaflow-core/src/addon/bundled.rs:70-92`),
  `install_native_packages()` przy starcie (`tentaflow/src/main.rs:705`), walidacja `runtime =
  "native"` + `validate_addon_id` (`bundled.rs:126`).
- Hooki cyklu życia: `REGISTRY: &[NativeAppHooks]` (`addon/native_apps.rs:79-115`),
  `NativeAppContext { db, addon_id, org_id, data_dir }` (`:16`), `package_of_instance()` (`:143`)
  parsuje `{package}-{8hex}`.
- Manifest `src/<app>/app-manifest.toml`: `[addon]`, `[application]` (`title_key`,
  `description_key`, `sort_order`), `[native]` (`singleton`, `routes`, `db_file`,
  `i18n_namespace`, `background_on_disable`), `[[permission]]` (`id`, `risk`, `default`).
  Parsowanie `[native]`: `addon/lifecycle.rs:3077-3100`.
- Instalacja instancji: `lifecycle::install_instance` (`:109`) → `install_native_instance`
  (`:209-317`): **egzekwuje singleton** (`:226-235`), mintuje id (`unique_instance_id`, `:478`),
  wpis w `addons`, `seed_permission_defaults`, `hooks.init` tylko na wspieranej platformie,
  `record_node_status`. Instalacja jest globalna dla floty; inne węzły inicjują przez reconcile
  (`addon/mod.rs:1288-1372`).
- Baza instancji: `addon/app_db.rs` — `open(main_db, org_id, addon_id, migrate)` (`:39`) czyta
  `native.db_file` z manifestu instancji i otwiera `<orgs>/<org>/addons/<addon_id>/<db_file>`
  (`fs_sandbox::addon_data_dir`, `fs_sandbox.rs:79`); PRAGMA WAL + pula odczytu; `close()` przed
  wymazaniem katalogu; `run_versioned_migrations` (`:130`).
- Bramka: `dispatch/app_gate.rs::require_app_permission(ctx, package_id, permission_id)` (`:20-59`)
  → matryca uprawnień `PermissionChecker`, jednolite `AppUnavailable` dla nie-adminów.
- **Uprawnienia są własnością platformy, nie aplikacji.** Katalog z `[[permission]]` manifestu
  (`id`, `display_name`, `description`, `risk`, `default`) trafia do `addon_permission_defaults`
  przy instalacji i przy każdym reconcile (`seed_permission_defaults`, `lifecycle.rs:1013`:
  `DO NOTHING`, edycja admina nigdy nie jest nadpisywana; `deny` nie ma wiersza). Wpisy matrycy
  są kluczowane **`addon_id` instancji** (`checker.check(&addon_id, user_id, permission_id, None)`,
  `app_gate.rs:52`), więc dwie instancje jednego pakietu mają dwie niezależne matryce. Hierarchia
  (`addon/permissions.rs:7`): admin bypass (rola `admin`/`is_admin`/grupa `admins`) > wpis per
  użytkownik > wpis per grupa (deny wygrywa) > default addona > deny. UI administratora:
  `www/js/modules/addons/permissions.js` (podzakładki per grupa / per użytkownik / default,
  tryby `allow`/`deny`/`inherit`, chip ryzyka) i `visibility.js` (grupy widzące kafelek,
  `admin_only`, katalog); `AddonPermissionChangedEvent` odświeża drugiego admina. Kafelek
  w `ReqAppsList` filtrowany przez `is_addon_visible_to_user` (`handlers.rs:9302-9326`).
  Code Studio i Projekty trzymają w matrycy tylko dostęp gruby (`code_studio.read/admin`,
  `project_studio.read/admin`) i mają własne tabele członków per workspace/projekt — bo tam
  jednostką współdzielenia jest workspace, nie instancja. W TentaQuant jednostką jest
  **instancja**, więc matryca wystarcza (§10).
- Protokół: jeden `MessageBody::<App>Body(<App>Payload)`, żądanie i odpowiedź jako warianty jednego
  enuma (`tentaflow-protocol/src/tentanas.rs:827`, `code_studio.rs:627`), `variant_name_of`
  w `dispatch/mod.rs`, handlery `#[handler]#[policy(UserSession)]#[observed]`
  (`dispatch/tentanas.rs:1952`).
- Frontend: `Router.register('<route>', Screen)` (`www/js/app.js:535-541`), kafelek z `ReqAppsList`
  (`dispatch/handlers.rs:9280-9390`, `AppEntryWire` w `message_body.rs:6140-6162`), CSS per
  aplikacja w `www/index.html:45-52`, i18n `apps.<id>.{name,desc}` + przestrzeń z manifestu
  w pięciu locale.
- Mesh: `dispatch/app_route.rs` — dashboard jako czyste proxy (`forward_to_node`, `:73`;
  `APP_ROUTE_SCOPE`, `:44`; `OP_TIMEOUT_SECS = 45`, `:38`), węzeł docelowy przelicza aktora
  z własnej bazy; **subskrypcje strumieniowe nie są forwardowalne** (`:20-22`).
- Wykonanie/strumienie Code Studio: `code_studio/exec/` (argv-only, grupa procesów, minimalne env),
  `code_studio/mesh_stream.rs` (okno kredytowe, `DEFAULT_INLINE_BUDGET = 4 MiB`, `REPLAY_FRAMES =
  512`, klucz (sesja, strumień, węzeł konsumenta, użytkownik)).
- Sidecar jako klient odwrotny: `reverse_requests = true` w manifeście usługi
  (`tentaflow-containers/agents/_services/teams-bot.toml:22`), `services/runtime/reverse_listener.rs`,
  bramka własności sesji `meeting/flow_turn.rs::lookup_owned_session` (CLAUDE.md „Meeting Bot”).
- Edytor: `tf-code-editor` bez zależności, tokenizery per język (`tf-code-editor.js:67-69`:
  python, rust, toml, …), `tf-terminal` (siatka VT po stronie serwera), `tf-diff`, `tf-canvas`
  (lista `DrawCommand`), `tf-bar-chart`, `tf-line-chart`, `tf-heatmap`, `tf-stream-chart`.
- Usługi Python: `_services/test-runner.toml` (Docker + `python-bundle`, parametry z bindingami
  env), bundle `tools/python/test-runner/{bundle.toml, server.py, requirements.lock, executor/}`.
- GPU węzła w heartbeacie: `MeshGpuMetric { index, usage_percent, vram_used_mb, vram_total_mb,
  temperature_c }` (`tentaflow-protocol/src/mesh.rs:171`). Wyboru węzła dokonuje UI (ML Studio:
  `dispatch/ml_studio.rs:2280-2384`).

### 2.2 Luki platformy do zamknięcia (multi-instance) — Faza 0

Wszystkie sześć zarejestrowanych aplikacji ma `singleton = true`; mechanizm instancji jest
generyczny, ale trzy miejsca zakładają jedną instancję na pakiet:

1. **Bramka rozwiązuje instancję po pakiecie.** `require_app_permission` woła
   `get_package_instance` (`db/repository.rs:12949-12960`: `ORDER BY is_enabled DESC LIMIT 1`) —
   dla dwóch instancji wybierze losową. Potrzebne
   `require_instance_permission(ctx, package_id, addon_id, permission_id)`: instancja
   z żądania, weryfikacja `addons.package_id == package_id` **i** `is_enabled`, dopiero potem
   matryca. Istniejąca funkcja zostaje dla singletonów; nowa nie jest kopią, tylko wspólnym rdzeniem
   z jawnym `addon_id` (reguła 4 z CLAUDE.md). Sama matryca **nie wymaga zmian** — jest już
   kluczowana instancją, a UI w Addons już edytuje ją per instancja.
2. **`app_db::open_for_package`** (`app_db.rs:59`) — zadania w tle bez id instancji. TentaQuant
   go **nie używa**: każde zadanie w tle (sondowanie jobów QPU, GC) iteruje po instancjach pakietu
   (`list_package_instances`), nie po „tej jednej”.
3. **Kafelek i trasa.** `ReqAppsList` emituje wiersz per instancja z `display_name`
   (`handlers.rs:9337-9342`), ale `target` to goła trasa (`:9343-9352`), a klik robi
   `Router.navigate(target)` bez parametrów (`apps-home.js:159-172`). `AppEntryWire` dostaje
   `#[serde(default)] instance_id`, klik przekazuje `{ instance }`, moduł czyta go z `params` i
   z `#/tentaquant?instance=<addon_id>` (wzór `setLocation`, `tentanas.js:213-227`). Aplikacje
   singletonowe nie zmieniają zachowania.
4. **Kreator instalacji** (`www/js/modules/addons/install-wizard.js`) musi przy `singleton = false`
   wymagać `display_name` i pozwalać na drugą instalację; `ReqDuplicate` już istnieje
   (`message_body.rs:5933-5950`). Dodatkowo kreator dostaje **kroki aplikacji**: pakiet natywny
   deklaruje w manifeście listę kroków (nowa sekcja `[[install_step]]`: id, tytuł i18n, formularz
   jako schemat pól, akcja po instalacji), które generyczny kreator renderuje po danych i matrycy
   i wykonuje przez `NativeAppHooks::install_step`. TentaQuant dokłada deploy `quantum-python`,
   wybór nodów GPU i test Bell 2q na każdej warstwie (Q14). To zmiana w Addons dla każdej kolejnej
   aplikacji wieloinstancyjnej, nie ekran prywatny TentaQuant.
5. **Komentarz o limicie 256 wariantów** przy `MessageBody` (`message_body.rs:7596`) jest
   nieprawdziwy (CLAUDE.md „Projekty”, tagi po NAZWIE) — nowy wariant `TentaQuantBody` dodajemy bez
   obaw; komentarz do usunięcia przy okazji.

### 2.3 Co reużywamy bez przepisywania

Platforma instancji (§2.1), `app_db` (pula per instancja), `SettingsCipher` (sekrety dostawców),
`SandboxLimits` + bollard `HostConfig` (kształt `test_runner`), `services/deploy` (start usług
python-bundle/Docker, parametry, `NCCL_P2P_LEVEL`), `reverse_listener` + `ReverseWiring`,
`app_route`, `mesh_stream` (po uogólnieniu), `tf-code-editor` (+ tokenizer `openqasm`),
`tf-bar-chart`/`tf-line-chart`/`tf-heatmap`, `log_bus` (postęp długich zadań), `AgentRunManager`
i flow engine (bloki hybrydowe), `ml_link.rs` jako wzór jednokierunkowego lustra ról,
`project_db.rs` jako wzór ograniczonej puli LRU, `archive.rs` jako wzór eksportu/importu.

### 2.4 Ograniczenia kształtujące plan

- Kod użytkownika jest **niezaufany** — inaczej niż w Code Studio, gdzie użytkownik pisze własny kod na
  własnej maszynie. Domyślny tryb musi być kontenerem tam, gdzie kontener jest.
- Wektor stanu rośnie jak 2^n: przeglądarka (wasm32, 4 GiB) kończy się na ~24 q, Core na ~28–30 q,
  jedno GPU 80 GB na ~33 q (double). Warstwy nie są opcją, tylko koniecznością — §4.2.
- Poświadczenia dostawców i **minuty QPU są pieniędzmi** (IBM $48–96/min). Każde wysłanie musi
  przejść przez Core, mieć kosztorys, limit i ślad audytowy.
- Sieć sandboxa: kod użytkownika nie może sam wołać `quantum.cloud.ibm.com` — nie tylko dla
  poświadczeń, ale żeby pula sekund konta laboratorium dała się w ogóle egzekwować.
- Brak `oq3_*` w wasm to ryzyko jednego spike'u; jeśli nie zbuduje się pod `wasm32`, przeglądarka
  dostaje **serializowany IR z Core** (Core parsuje, przeglądarka symuluje), a parsowanie lokalne
  ogranicza się do edycji obwodu w edytorze wizualnym. Plan nie zależy od wyniku spike'u.
- Python w przeglądarce to drugi spike (F, §4.6): Qiskit nie ma kółka Pyodide, więc T0 dla komórek
  Python zależy od tego, czy `qiskit._accelerate` zbuduje się pod Emscripten tą samą recepturą, co
  `rustworkx`. Plan ma dla tego fallback bez Pyodide (jądro T2 wykonuje Python, przeglądarka liczy
  obwód), więc wynik spike'u zmienia zakres T0, nie architekturę.

### 2.5 Dystrybucja usług: jak działa `python-bundle` i czego brakuje

Mechanizm, który TentaQuant ma dostać „w pliku razem z programem”, już istnieje:

- **Bundle jest w binarce.** `build.rs::pack_container_contexts` (`tentaflow-core/build.rs:1184`)
  pakuje `tentaflow-containers/` (+ `tentaflow-protocol`, `-transport`, `-voice`, `vendor`) do
  `container_bundle.tar.gz`, wbudowanego przez `include_bytes!` (`deploy/bundle.rs:14`);
  `paths::ensure_app_dirs` (`paths.rs:805`) rozpakowuje go przy starcie do
  `<home>/containers/` gdy zmienił się `.bundle_hash`. Wykluczone z tar: `*.pth *.onnx *.gguf
  *.safetensors`, katalogi `target/ node_modules/ .venv/` (`build.rs:1240-1264`).
- **Python nie jest systemowy.** `python_venv.rs::ensure_python` (`:457`) pobiera
  python-build-standalone (pin `PBS_DATE = 20260408`, 3.11/3.12/3.13) i `uv 0.5.14` (`:493`) do
  `<cache>/`; venv przez `python -m venv`; szablon `<cache>/bundle-templates/<engine>/<id>/venv`
  i instancja `<cache>/bundle-instances/<engine>/<name>` jako hardlink-klon (`:770-1005`).
  Tożsamość szablonu = hash `bundle.toml` (bez `[launch]`) + plików bundle'a (`:853`), więc zmiana
  wymagań przebudowuje środowisko, zmiana komendy startu nie.
- **Instalacja:** `requirements.lock` (`:1131`), `[bundle] source = pypi|git|vllm-metal`,
  `extras`, `force_pins`; albo tryb uv-native (`pyproject.toml` + `uv.lock` → `uv sync --frozen`,
  `:1041-1052`). **Wybór wheeli per GPU** przez `[[install_variants]]` z `backend =
  cuda|cuda-<arch>|rocm|metal|xpu|cpu` i `extra_index` (`pick_install_variant`, `:1497`), tag
  z `system_check::GpuSnapshot::cuda_arch_tag()` (`system_check/mod.rs:175`): NVIDIA przez
  `nvidia-smi`, AMD przez `rocminfo`, Intel XPU, Metal = kompilacja na macOS/aarch64, Vulkan
  przez `vulkaninfo --summary` (`:534`). `TORCH_CUDA_ARCH_LIST` wstrzykiwany automatycznie.
- **Rejestracja i restart:** deploy = wiersz w `services` (`python_bundle.rs:559`), transport
  `HttpDirect` na `127.0.0.1:<port>`, sonda gotowości bez limitu czasu (tylko śmierć procesu ją
  przerywa), supervisor sonduje `/health` (infra: dowolna odpowiedź HTTP), a `services.pinned`
  (domyślnie `true` poza iOS/Android, `deploy/mod.rs:1364`) uruchamia usługę po restarcie Core
  (`supervisor.rs::auto_start_pinned`, `:485`).
- **Granica Rust↔Python to wyłącznie proces + HTTP.** W repo **nie ma** pyo3 ani maturina
  (zero trafień). Moduł Pythona z naszego crate'a (§6.4) będzie pierwszym takim artefaktem.

**Luki do zamknięcia (Faza 0/2):**

1. **Nic nie deployuje usługi z hooka instalacji aplikacji.** `NativeAppHooks` ma tylko
   `init/teardown_plan/teardown` z `NativeAppContext { db, addon_id, org_id, data_dir }`
   (`native_apps.rs:63`) — bez `PortAllocator` i `SettingsCipher`, których wymaga
   `deploy::deploy()` (`services/deploy/mod.rs:507`). Wszystkie dotychczasowe konsumenty usług
   (test-runner w Projektach, searxng w web_research, ksmbd w TentaNas) **odkrywają**, nie
   deployują. Rozwiązanie: `NativeAppContext` dostaje uchwyt `services: &AppServices` (porty,
   szyfr, `log_bus`), a hook `init` **zleca** deploy jako zadanie w tle (`create_deploy_job` +
   `deploy` w `tokio::spawn`, postęp na `log_bus` jak migracja magazynu), nie blokując instalacji
   — pierwszy deploy `quantum-python` pobiera setki MB wheeli i trwa minuty. Instancja jest
   „zainstalowana” od razu, kafelek pokazuje stan usługi (`provisioning → ready`), T0/T1 działają
   bez usługi. Reconcile na innych węzłach (`addon/mod.rs:1288-1372`) robi to samo per węzeł
   z GPU, a instancja ma ustawienie „węzły, na których stawiać usługę”.
2. **`copy_bundle_files` kopiuje tylko pliki płaskie** (`python_venv.rs:1776-1790`) — bundle
   z pakietem w podkatalogu (dziś `test-runner/executor/`, jutro `tentaquant_sdk/`) nie startuje
   natywnie (`ModuleNotFoundError`). Kopiowanie rekurencyjne to warunek, nie opcja.
3. **`[requires] gpu_memory_gb / disk_gb / cuda` nie są nigdzie czytane** (`python_venv.rs:143-147`)
   — egzekwowane jest tylko `platforms`. Dla `quantum-python` (Aer + PennyLane + wheel
   `tentaquant_sim`) potrzebny preflight dysku przed pobieraniem.
4. **Lokalne wheel-e.** Nasz moduł `tentaquant_sim` (pyo3) nie może iść przez PyPI ani gita;
   bundle dostaje klucz `local_wheels = ["wheels/tentaquant_sim-<ver>-<tag>.whl"]` rozwiązywany
   względem katalogu bundle'a, a `release.yml` buduje wheel per platforma (maturin) i wkłada go
   do `tentaflow-containers/tools/python/quantum-python/wheels/` przed `pack_container_contexts`.
   Wykluczenia tar nie obejmują `*.whl`.
5. **Windows w ścieżce natywnej**: `spawn_engine` skleja `PATH` z `venv/bin` i `:`
   (`python_venv.rs:2168-2180`) mimo poprawnego `venv_bin()` — do poprawienia, zanim
   `platforms` obieca `windows-x86_64`.

**Czego to nie daje: pełnego offline.** Pierwszy deploy pobiera Pythona, uv i wheel-e z sieci
(jak każdy python-bundle dziś). Instalacja bez internetu wymagałaby gotowych archiwów venv per
platforma i backend (wiele GB) jako assetów release — możliwe później jako opcja „pakiet
offline”, nie w v1.

---

## 3. Model pojęciowy i maszyny stanów

### 3.1 Byty

| Byt | Zakres | Opis |
|---|---|---|
| **Instancja** (laboratorium) | platforma | Wiersz `addons` z `package_id = "tentaquant"`, własny katalog i `tentaquant.db`, `display_name` |
| **Członek** | instancja | Użytkownik org, któremu matryca instancji przyznaje `quant.read` (przez grupę lub wpis własny); „rola” = zestaw przyznanych uprawnień (§10.2). Nie ma wiersza członka w aplikacji |
| **Projekt** | instancja | Kontener plików i notatników. Model własności jak w ML Studio: twórca jest właścicielem, projekt jest **domyślnie prywatny** i widzi go tylko właściciel; właściciel udostępnia go wybranym osobom z rolą `editor`/`viewer` albo całemu laboratorium (`visibility = lab`, tylko do odczytu, np. materiały opiekuna). Rola `viewer` uruchamia komórki **tylko na T0** w przeglądarce, bez zapisu wyniku do projektu i bez runu (§10.3). Udostępnienie nie omija matrycy: osoba bez `quant.read` w tej instancji nie zobaczy projektu, dopóki admin nie da jej dostępu w Addons. Link do Projektu z Project Studio opcjonalny (§13.7) |
| **Notatnik** | projekt | Uporządkowana lista komórek `code` (python) / `markdown` / `circuit` (OQ3 + układ wizualny); wersjonowany append-only jak `test_case_versions` |
| **Obwód** | komórka/plik | OQ3 kanonicznie + `layout_json` (pozycje bramek w edytorze wizualnym, adnotacje). Edytor wizualny i tekst są **dwiema projekcjami jednego IR** |
| **Program** | plik | `.py` używający SDK `tentaquant`; jednostka uruchamiania T2/T3 poza notatnikiem |
| **Target** | instancja/węzeł | `browser` · `core:<node>` · `python:<node>` · `gpu:<node>[:idx]` · `qpu:<provider_id>:<backend>`; klasa targetu = tier |
| **Sesja jądra** | node-local | Żywy `ipykernel` w usłudze na węźle, przypięty do (instancja, użytkownik, notatnik); ma tier, limity, TTL |
| **Run** | instancja | Jedno wykonanie (komórki, obwodu, programu, kata) na targecie; artefakty w CAS |
| **Job QPU** | instancja | Podzbiór runu: zlecenie u dostawcy z `provider_job_id`, kolejką, zużyciem, kosztem |
| **Konto QPU** | użytkownik / instancja | Token IBM: osobisty (`scope = user`, tylko właściciel, bez puli) albo konto laboratorium (`scope = instance`, najwyżej jedno, `quant.admin`) |
| **Pula** | instancja | Sekundy QPU konta laboratorium na okres + domyślny limit per osoba + nadpisania per osoba; runy z kont osobistych nie schodzą z puli |
| **Przykład** | wbudowany / instancja | Katalog algorytmów w wariantach cpu/gpu/qpu + obwód + README; instancja może dodawać własne |
| **Kata** | wbudowana | Jedno z 24 zadań kursu: treść + weryfikacja + rozwiązanie wzorcowe; postęp, punkty i seria per użytkownik |

### 3.2 Maszyny stanów

**Run**: `created → queued → running → { succeeded | failed | cancelled }`; dla targetów QPU
`running` ma podstany `compiling → submitted → provider_queued → provider_running → collecting`.
Run osierocony restartem Core: znacznik `register_local_run` jak w ML Studio — Core, który go nie
nadzoruje, zamyka go przez `reconcile_orphan_local_run`, bez heurystyk czasowych.

**Job QPU**: `estimated → awaiting_approval → approved → submitted → queued → running →
{ completed | failed | cancelled | expired }`. `awaiting_approval` tylko dla konta laboratorium; `expired` gdy zgoda nie nadeszła w oknie (domyślnie 24 h) — nic nie zostało wysłane,
nic nie kosztuje.

**Sesja jądra**: `starting → idle ⇄ busy → { stopping → stopped | crashed }`; `idle` z TTL (domyślnie
30 min bez wykonania) → `stopped`; `busy` z limitem czasu komórki (domyślnie 300 s, T3 900 s) →
przerwanie (`interrupt_request`), po drugim przekroczeniu `kill`.

**Zgoda na QPU**: `requested → { granted | denied | expired }`, decyzja przypięta do
(job, aprobujący, powód); zgoda nie jest przenoszalna na inny job ani inny backend.

---

## 4. Warstwy wykonania (tiers)

### 4.1 Pięć warstw, jeden artefakt

| Tier | Gdzie | Silnik | Wejście | Dla kogo |
|---|---|---|---|---|
| **T0 browser** | przeglądarka, WASM | `tentaflow-quantum` (SV/stabilizer) | obwód (IR) | edytor wizualny, żywy stan przy każdej zmianie, katy „na żywo” |
| **T1 core** | Core, węzeł wybrany | `tentaflow-quantum` natywnie (rayon) | obwód OQ3 | ocena kat, bloki flow, runy bez Pythona, podgląd serwerowy |
| **T2 python** | usługa `quantum-python` | Qiskit/Aer CPU, PennyLane, `qdk`, **`tentaquant_sim` (nasz crate przez pyo3: CPU + `cuda`/`wgpu`)** | komórki/programy | nauka Qiskita, pełne API, transpile, symulacja z szumem Aer |
| **T3 gpu** | GPU wybranego węzła: w procesie Core (obwody) lub w jądrze T2 przez wheel `tentaquant_sim` (komórki) | `tentaflow-quantum` backend `cuda` (NVIDIA, PTX przez `cudarc`) / `wgpu` (Vulkan: AMD/Intel/NVIDIA bez CUDA; Metal: Apple) | obwód OQ3 / obwody z SDK | 28–32 q, benchmark tierów, `tentaquant.backend("gpu")` |

Od strony Pythona GPU jest **jedno**: `tentaquant.backend("gpu")` zwraca `BackendV2` Qiskita,
który liczy na `tentaquant_sim` w procesie jądra (`cuda` gdy węzeł ma NVIDIA i sterownik, inaczej
`wgpu`), a gdy węzeł jądra nie ma GPU, wysyła obwód kanałem zwrotnym na T3 innego węzła. Precyzja:
`cuda` liczy `complex64` i `complex128`, `wgpu` tylko `complex64` (§6.3). `backend.describe()`
mówi, co jest pod spodem — użytkownik ma to widzieć, nie zgadywać. cuStateVec, CUDA-Q i Aer-GPU
nie są targetami TentaQuant (decyzja §18.17). Kod klasyczny na GPU idzie przez CuPy (NVIDIA,
AMD-ROCm), Numbę (NVIDIA) i PyTorch (NVIDIA, ROCm, MPS na Apple, XPU na Intel) — tu jednolitości
producentów nie ma i przykłady mówią wprost, które wersje działają na którym sprzęcie.
| **T4 qpu** | broker Core → dostawca | IBM Qiskit Runtime (v1) | obwód ISA (OQ3) lub payload primitives | prawdziwy sprzęt, porównanie z symulacją |

Obwód z edytora wizualnego biegnie na T0 natychmiast, na T1/T2/T3 po kliknięciu, na T4 po
kosztorysie i bramce. Program Python biegnie na T2 (z GPU przez T3), a z niego — przez SDK — obwody trafiają na
dowolny tier, w tym T4.

### 4.2 Pojemność (wektor stanu, `complex64` = 8 B/amplituda; double = ×2)

| Kubity | Pamięć | Warstwa |
|---|---|---|
| 20 | 8 MiB | T0 (bez zauważalnego opóźnienia) |
| 24 | 128 MiB | T0 górna praktyczna granica (wasm32 4 GiB, ale JIT i kopie) |
| 26 | 512 MiB | T1 |
| 28 | 2 GiB | T1 górna granica domyślna (`max_qubits_core`, konfigurowalne) |
| 30 | 8 GiB | T2 Aer CPU na węźle z RAM; T3 |
| 32 | 32 GiB | T3 jedno GPU ≥ 40 GB (`complex64`); przez shardowanie buforów `wgpu` albo blokowo z wymianą do RAM (wolno, jawnie) |
| >32 | — | poza v1 (wiele GPU, sieci tensorowe — §6.5) |

Stabilizer (Clifford): O(n²) bitów → tysiące kubitów na T0/T1; edytor sam rozpoznaje obwód
Clifforda i proponuje tryb. Każdy tier ma `max_qubits` w konfiguracji instancji; przekroczenie to
błąd walidacji **przed** startem, z podpowiedzią wyższego tieru — nie OOM w trakcie.

### 4.3 Sesje jądra i protokół wyników

- Usługa (`server.py`, FastAPI, wzór `test-runner`) zarządza jądrami przez `jupyter_client`:
  `POST /sessions` (tier, limity, katalog roboczy, `session_token`), `POST /sessions/{id}/execute`
  (kod, `cell_id`), `POST /sessions/{id}/interrupt`, `DELETE /sessions/{id}`,
  `GET /sessions/{id}/stream` (SSE; komunikaty iopub jako JSON: `stream`, `display_data`,
  `update_display_data`, `execute_result`, `error`, `status`, `execute_reply`).
- Core przepisuje komunikaty na `TentaQuantPayload::KernelEvent` (§11.2) i trwale zapisuje
  wyjścia komórki do `cell_outputs` (mime bundle) oraz duże dane (`image/png`, `application/json`
  > 64 KiB) do CAS z referencją. Format mime bundle Jupytera jest **formatem zapisu**, więc eksport
  `.ipynb` to serializacja, nie konwersja.
- Własne typy mime: `application/x-tentaquant-state+json` (wektor stanu / gęstości, do
  `tf-bloch-sphere`, amplitud), `application/x-tentaquant-counts+json` (histogram),
  `application/x-tentaquant-circuit+json` (IR obwodu do `tf-quantum-circuit` — ten sam JSON,
  który zwraca `parse()`; renderer nie parsuje OQ3, więc `+qasm3` opisywałby nie te bajty). SDK
  emituje je przez
  `IPython.display` — zwykły `print` nadal działa.
- Interakcja odwrotna (`input_request`/`stdin`) **nie jest wspierana** — komórka z `input()` dostaje
  `EOFError`; upraszcza to model i sandbox.

### 4.4 SDK `tentaquant` w jądrze

Pakiet Python instalowany w bundle'u (`tools/python/quantum-python/tentaquant_sdk/`), importowany
w jądrze jako `tentaquant as tq`. Powierzchnia v1:

- `tq.run(circuit, target="gpu"|"cpu"|"core"|"qpu:ibm:ibm_kingston", shots=1024, **opts) ->
  RunResult` — przyjmuje `QuantumCircuit` (Qiskit), OQ3 `str`, `cudaq` kernel, obiekt z
  `to_qasm3()`; dla T2 wykonuje lokalnie w tej samej usłudze (Aer / `tentaquant_sim`), dla
  T1/T3-Core/T4 wysyła przez brokera usługi do Core.
- `tq.estimate(circuit, observables, target=...)`, `tq.sample(...)` — odpowiedniki primitives.
- `tq.targets()` — lista dostępnych targetów z pojemnością, kolejką i — dla konta laboratorium —
  pozostałą pulą sekund (z Core).
- `tq.show(state|counts|circuit)` — emisja własnych typów mime.
- `tq.hybrid.minimize(cost_fn, x0, optimizer="cobyla"|"spsa"|"adam", ...)` — cienka pętla z
  wykresem zbieżności aktualizowanym przez `update_display_data`; nie ukrywa optymalizatora,
  tylko standaryzuje raportowanie.

SDK nigdy nie widzi poświadczeń: żądanie T4 zawiera obwód i parametry, a `session_token` (jednorazowy
per sesja, wydany przez Core przy `POST /sessions`) uwierzytelnia tylko **sesję**, nie użytkownika
poza nią.

### 4.5 Kanał zwrotny usługi

Usługa deklaruje `reverse_requests = true`; Core przy dołączeniu `QuicServiceHandle` z
`ReverseWiring` przyjmuje `open_bi` (`reverse_listener.rs`). Nowe warianty żądań odwrotnych:
`QuantumRun`, `QuantumTargets`, `QuantumJobStatus` — każde związane z sesją jądra przez
`lookup_owned_kernel_session(service_name, session_token)`: usługa może działać tylko w imieniu
sesji, którą Core jej założył (dokładnie wzór `meeting/flow_turn.rs::lookup_owned_session`).
Wszystko inne z tej usługi jest `Unauthorized`; **Core nie jest anonimowym proxy** — także dla
symulacji.

### 4.6 Python w przeglądarce: Pyodide (ścieżka główna) i „jądro liczy, przeglądarka wykonuje” (fallback)

T0 ma dwa silniki: `tentaflow-quantum` (wasm-bindgen) dla obwodów — jak w §4.1 — oraz **Pyodide**
dla komórek Python. Decyzja: próbujemy Pyodide, ale fallback jest projektowany i budowany
równolegle, bo Qiskit nie ma dziś kółka wasm (§1.5) i nikt poza nami go nie zbuduje.

**T0-py (Pyodide).**

- Worker `tentaquant-py.worker.js` ładuje Pyodide z Core: `www/vendor/pyodide/<ver>/` osadzone
  w binarce jak reszta `www` (żadnego CDN — dashboard nie wysyła dziś CSP, ale zasada „wszystko
  w pliku” i tak wyklucza obce hosty). Zestaw pakietów: rdzeń Pyodide, numpy, scipy, matplotlib
  (ładowany leniwie przy pierwszym imporcie), `rustworkx` (kółko PyPI `pyemscripten`), `qiskit`
  (nasze kółko ze spike'u F), SDK `tentaquant` (czysty Python — **ten sam pakiet**, co w jądrze
  T2, z warstwą transportu wybieraną w runtime).
- Symulatora nie kompilujemy drugi raz: `tentaquant_sim` w Pyodide to cienki moduł Pythona
  wołający przez `pyodide.ffi` ten sam `tentaflow-quantum` wasm, który napędza Studio obwodów;
  wektor stanu wraca jako `Float32Array` → `numpy` bez kopii. `BackendV2` z SDK opakowuje go
  identycznie jak natywne `tentaquant_sim`, więc `tentaquant.backend("auto")` w przeglądarce
  wybiera T0-py, gdy obwód ma ≤ 20 kubitów, a kod nie importuje niczego spoza zestawu.
- Ładowanie dopiero przy pierwszej komórce Python w trybie T0 (nie przy otwarciu notatnika),
  z paskiem postępu; pliki wersjonowane w ścieżce i serwowane z `Cache-Control: immutable`.
  Rozmiar pakietów i cold-start mierzy spike F — plan nie podaje liczb, których nie zmierzył.
  Domyślnie wyłączone na telefonach (§13.5) i przy `navigator.deviceMemory < 4`.
- Czego T0-py nie umie i jak to wykrywamy: importy spoza zestawu (`qiskit_aer`, `pennylane`,
  `cudaq`, `torch`, `cupy`), obwody > 20 kubitów (§4.2), `input()`, sieć. Statyczny skan
  importów (`ast`, w Pyodide, tanie) **przed** wykonaniem plus `ImportError` w trakcie → komórka
  dostaje stan `needs_kernel` z jednym przyciskiem „Uruchom na T2 · <węzeł>”; target `auto`
  robi to sam, gdy usługa działa. Żądanie QPU z T0-py (`tq.run(target="qpu:...")`) nie wychodzi
  z Pythona: SDK oddaje obwód do JS, a dashboard wysyła zwykłe `QpuSubmit` protokołem binarnym
  — ta sama bramka limitów i zgód, co dla każdego innego klienta (§7.3).
- Przerywanie komórki: Pyodide przerywa przez `SharedArrayBuffer` (`setInterruptBuffer`), co
  wymaga cross-origin isolation (COOP `same-origin` + COEP `require-corp`) na całym dashboardzie.
  Core tych nagłówków nie wysyła; włączenie ich to audyt wszystkiego, co dashboard osadza (iframe
  Quirk z §1.5, callbacki OAuth, obrazy z addonów). Alternatywa: `worker.terminate()` + ponowne
  załadowanie (stan jądra przepada, UI mówi o tym wprost). Rozstrzyga spike F (§18, pkt 16).
- Wersje są jednym numerem: Pyodide ↔ Python (314 = 3.14) ↔ numpy/scipy z dystrybucji Pyodide
  (nie z PyPI) ↔ nasze kółka `pyemscripten_<abi>`; ABI Pyodide zmienia się z wydaniem
  (`2026_0`), więc podbicie Pyodide = przebudowa kółek. Wersja Qiskita w T0-py **musi** równać
  się wersji w jądrze T2 — inaczej „ten sam kod na każdej warstwie” jest nieprawdą.

**Fallback: jądro liczy, przeglądarka wykonuje.** Zawsze obecny, niezależny od Pyodide, jedyna
ścieżka, jeśli spike F padnie.

- Kernel T2 wykonuje Python normalnie. `backend.run(qc)` z celem `browser` (albo `auto`
  z małym obwodem, gdy sesja dashboardu żyje) nie symuluje w jądrze: SDK serializuje obwód do IR
  i wysyła kanałem zwrotnym usługi nowy wariant `QuantumBrowserRun{session_token, circuit_ir,
  shots, want_state}` (obok `QuantumRun`, §4.5, ta sama bramka `lookup_owned_kernel_session`).
  Core kieruje żądanie do sesji WebSocket **właściciela sesji jądra** jako `KernelEvent`
  `browser_run_request{request_id, ...}`; przeglądarka liczy w `tentaflow-quantum` wasm i odsyła
  `KernelBrowserRunResult{request_id, counts, state?}`; Core odpowiada na strumieniu zwrotnym
  i jądro wznawia. Z punktu widzenia Pythona to zwykłe `Job.result()`.
- Limit 30 s na odpowiedź. Gdy sesja dashboardu zniknęła (karta zamknięta) albo limit minął,
  Core wykonuje obwód sam na T1 i znaczy run `executed_on = core` z powodem
  `browser_unavailable` — kod użytkownika nie ma prawa się wywrócić dlatego, że zamknął kartę.
- Sesja jądra na innym węźle niż dashboard: żądanie idzie mesh'em do węzła, do którego przypięty
  jest dashboard (relay strumieni §11.3), nie bezpośrednio z usługi.
- Bezpieczeństwo: przeglądarka dostaje wyłącznie IR i odsyła liczby; Core sprawdza kształt wyniku
  (`shots` × `n_qubits`, suma counts = shots). Nic z tego nie omija limitów, bo limity są na T4.
- Po co to także przy działającym Pyodide: panel stanu (Bloch, amplitudy) po runie z jądra bez
  przesyłania wektora stanu z serwera (20 q = 8 MiB, 24 q = 128 MiB) — przeglądarka ma go u siebie.

**Polityka.** Spike F w Fazie 0 (budżet 2 tygodnie): `qiskit._accelerate` pod
`wasm32-unknown-emscripten` recepturą `rustworkx` (nightly Rust, emsdk, `pyodide-build`),
prawdopodobne łatki: `getrandom` (to robi PR #15484), `rayon` wyłączony feature'em. Kryteria:
`import qiskit`, `QuantumCircuit`, `transpile` na `GenericBackendV2`, `Statevector` działają
w Pyodide 314 w Chrome, Firefox i Safari; zmierzone: rozmiar pobrania, cold-start, czas
`transpile` 4 q. Wynik to ADR „T0-py: tak/nie”.

- **Tak** → T0-py wchodzi w Fazie 2 razem z SDK; kółko Qiskit buduje osobny job `release.yml`
  (nightly Rust + emsdk, artefakt do `www/vendor/pyodide/`), przypięty do wersji Qiskita z jądra.
  Fallback też w Fazie 2 (kanał zwrotny już tam jest).
- **Nie** → T0 dla Pythona = wyłącznie fallback; komórka Python bez usługi pokazuje „potrzebuje
  jądra” zamiast udawać; spike ponawiamy, gdy PR #15484 dojrzeje. Mockupy Q06/Q10/Q11 są
  narysowane dla wariantu „tak” i w tym wariancie plakietka `T0 · Pyodide` staje się
  `T2 → przeglądarka`.

UI: w selekcie targetu (Q06) „Przeglądarka · obwód” i „Przeglądarka · Python (Pyodide)” to dwie
pozycje, bo mają różne ograniczenia; plakietka komórki mówi, który silnik liczył.

---

## 5. Hybryda CPU + GPU + QPU

### 5.1 Trzy poziomy, uczciwie nazwane

| Poziom | Ziarnistość | Mechanizm | Status |
|---|---|---|---|
| **Program** | ms–s | Python w jądrze, `tq.run(target=...)` per etap; pętla wariacyjna klasyczna, etapy kwantowe kierowane do tieru | v1 |
| **Pipeline** | s–min | Flow Builder: bloki `quantum_run`, `quantum_program`, `quantum_target_switch`; węzły mesh z GPU; wynik do LLM/raportu | v1 (F5) |
| **Czas rzeczywisty** | µs | NVQLink / `cudaq-realtime`, kontroler FPGA | **poza zakresem**, jawnie w UI |

„Część na jednych urządzeniach, część na innych” realizuje poziom 1 i 2. Przykład (§12.2 „VQE
H₂”): gradient i optymalizator na CPU, ewaluacja Hamiltonianu na GPU (`tentaquant.backend("gpu")`, rząd
1000× szybciej niż CPU przy 20 q), ostatnie 3 iteracje z najlepszymi parametrami na QPU z porównaniem
energii — jeden plik, trzy targety.

### 5.2 Bloki flow (`flow_engine/node_adapters/`)

- `quantum_run` (1-in/1-out, kategoria `service`): wejście `FlowValue::Text` (OQ3) lub `Json`
  (`{qasm3, params}`), konfiguracja `target`, `shots`, `observable`; wyjście `Json` (counts /
  expectation / metadane runu). Dla `qpu:*` przechodzi tę samą bramkę limitów co UI —
  **flow nie omija zgody**; brak zgody → `InteractionGate` jak w `patch_review.rs` (blok czeka na
  decyzję, run flow się wstrzymuje).
- `quantum_program`: uruchamia program (plik z projektu) w nowej sesji jądra na wskazanym tierze,
  strumieniuje wyjścia do `log_bus`, zwraca ostatni `execute_result` + artefakty.
- `quantum_target_switch`: wybór targetu na podstawie wejścia (liczba kubitów, dostępność GPU
  w mesh, pozostały limit) — deterministyczna reguła, nie model.
- Bloki są rejestrowane przez provider katalogu usług (`services/catalog/provider.rs`), jak bloki
  Code Studio, i dostępne w Flow Builderze tylko, gdy instancja TentaQuant istnieje i użytkownik ma
  `quant.run`.

### 5.3 Placement

Węzeł dla T2/T3 wybiera użytkownik (lista węzłów z `MeshGpuMetric`, wolny VRAM, wersja usługi,
stan `instance_status`). `quantum_target_switch` i `tq.targets()` mają tę samą tabelę.

`device="auto"` (notatnik, Studio, SDK) to jedna deterministyczna reguła, ta sama w Core i w SDK:

1. obwód ≤ 20 q i wywołanie z przeglądarki → T0;
2. ≤ `max_qubits_core` (domyślnie 28) → T1 na węźle notatnika;
3. powyżej, gdy użytkownik ma `quant.run.gpu` i jakiś węzeł z GPU jest online → T3 na węźle
   z największym wolnym VRAM w heartbeacie;
4. komórka Python, której importy wymagają jądra → T2 (GPU z jądra przez `tentaquant_sim`);
5. nigdy QPU — T4 jest zawsze jawnym wyborem z kosztorysem.

Wynik reguły pokazuje UI przed startem („auto → T1 · node-a”). Kolejka, szacowanie czasu
i scheduler po obciążeniu — Faza 7, po zebraniu metryk z Q09.

---

## 6. Crate `tentaflow-quantum`

### 6.1 Zakres

- `parse`: OQ3 → IR przez `oq3_parser`/`oq3_semantics` 0.7.0; wspierany podzbiór: deklaracje
  `qubit[n]`/`bit[n]`, bramki standardowe (`stdgates.inc`), `gate` użytkownika (inline),
  `measure`, `reset`, `barrier`, `if` na bitach, `for` z zakresem stałym (rozwijane), parametry
  `input float`. Bez `defcal`, `duration`, `extern`, `while`, `box` — jasny błąd walidacji z linią.
- `ir`: obwód jako lista operacji na indeksach; `to_qasm3()` deterministyczne (round-trip test);
  `layout_json` edytora trzymany obok, nie w IR.
- `sim::statevector`: `complex64`/`complex128`, bramki 1–2-kubitowe przez indeksowanie bitowe,
  fuzja sąsiadujących bramek 1-kubitowych, `rayon` natywnie, single-thread w wasm; próbkowanie
  shotów, wektor stanu, prawdopodobieństwa, wartość oczekiwana obserwabli Pauliego, redukowana
  macierz gęstości 1–2 kubitów (do sfery Blocha); krok po kroku (`step()` po bramce) dla edytora;
  `step_fraction(t)` (ułamek bramki przez `U^t` z rozkładu własnego), `keyframe(step, opts)`
  (Bloch wszystkich kubitów, ρ par, `top_k` z partnerami — §13.6), `mutual_information`,
  `concurrence`.
- `sim::stabilizer`: tableau Aaronsona–Gottesmana dla obwodów Clifforda; wykrywanie „obwód jest
  Cliffordem” w IR.
- `sim::noise`: kanały Krausa na obwodzie idealnym — depolaryzacja 1q/2q, tłumienie amplitudy
  i fazy, błąd odczytu per kubit; model budowany z kalibracji IBM (`/backends/{id}/properties`:
  błędy bramek, T1/T2, czas bramki, błąd odczytu) tym samym przepisem, co `NoiseModel.from_backend`
  w Aer, albo ręcznie (jednolity `p`). Macierz gęstości do 14 q (4^n amplitud), powyżej trajektorie
  kwantowe (próbkowanie Krausa per shot) na tym samym statevectorze, więc działa też na `cuda`/`wgpu`.
  Test złoty: counts z Aer z tym samym modelem, TVD < 0,02 przy 10⁴ shotach. To jest „symuluj
  z szumem” na T1/T3 (Q13, katy 21–24); T2 używa Aer.
- `grade`: porównanie stanów do fazy globalnej, porównanie unitariów (dla kat), odległość
  rozkładów (TVD) między counts — ocena kat i porównanie symulacja↔QPU.
- `export`: OQ3 (kanoniczny), Qiskit-Python — generatory tekstu z IR; QIR **nie tu** (usługa
  Python, `qbraid-qir`); dialekty innych dostawców razem z nimi (§7.4).
- `wasm` (feature): `wasm-bindgen` API: `parse`, `simulate(ir, opts)`, `step`, `bloch(q)`,
  `counts(shots)`; ten sam pipeline `build.rs` co `tentaflow-protocol-wasm` → `www/js/quantum/
  quantum_glue.{js,wasm}`; brak pliku = build.rs pomija, a Studio obwodów przechodzi na T1 przez
  protokół (jak dashboard bez `wasm_glue`).

### 6.2 Poprawność

Testy złote: zestaw ~60 obwodów (Bell, GHZ, QFT 3–8 q, Grover 2–4 q, teleportacja z `if`,
losowe Clifford, losowe uniwersalne 10 q) z wektorami stanu i counts (seed) wygenerowanymi przez
Aer 0.17.2 i zapisanymi w `tests/golden/*.json`; test porównuje amplitudy (`1e-6`) i rozkłady.
Test równości natywny ↔ wasm na tym samym zestawie (`wasm-bindgen-test`). Round-trip OQ3.
Benchmark `benches/statevector.rs` (kubity × głębokość) — liczby do `docs/BENCH_TENTAQUANT_SIM.md`,
nie do CLAUDE.md.

### 6.3 Backendy GPU: układ jak w ggml (cpu / wgpu / cuda)

`sim` ma jeden trait `Backend` (alokacja stanu, `apply(gate_batch)`, `probs`, `sample`,
`readback`) i trzy implementacje, dokładnie jak ggml w llama.cpp ma `cpu`/`vulkan`/`cuda`/
`metal`. Wybór w runtime, nie w czasie kompilacji: `cuda` gdy `libcuda.so`/`nvcuda.dll` da się
załadować i jest urządzenie, inaczej `wgpu` (Vulkan na NVIDIA/AMD/Intel, Metal na Apple, DX12 na
Windows bez Vulkana), inaczej `cpu`. Jedna binarka i jeden wheel obsługują wszystkie karty —
ROCm, MPS i XPU nie pojawiają się nigdzie jako zależność, bo Vulkan/Metal je zastępują.

**`cuda`** (feature `cuda`, domyślnie włączona na Linux/Windows): `cudarc` z `dynamic-loading`
(ten sam układ co `forge-hal` w `tentaflow-infer`: sterownik ładowany `dlopen`, brak sterownika
= brak backendu, nie błąd linkowania), kernele jako PTX wkompilowane `include_bytes!` i JIT-owane
przez sterownik — zero CUDA Toolkit u użytkownika. Kernele bramek pisane w CUDA C (nvcc tylko na
maszynie budującej, `sm_80+` jak w `forge-kernels`) albo w Mojo przez istniejący `build_kernels`
pipeline; w obu wypadkach artefakt to PTX w repo. Statevector nie potrzebuje cuStateVec:
bramka to memory-bound pętla po parach amplitud i własny kernel osiąga to samo pasmo; cuStateVec
nie jest zależnością ani crate'a, ani SDK.

**`wgpu`** (feature `wgpu`): ten sam IR, stan jako `vec2<f32>` w buforach storage, jeden kernel
WGSL na klasę bramki (1-kubitowa, kontrolowana, diagonalna, permutacyjna, pomiar/normalizacja,
redukcja prawdopodobieństw), fuzja kolejnych bramek 1-kubitowych na tym samym kubicie w jedną
macierz 2×2 przed wysłaniem, obliczanie `shots` przez redukcję prefiksową i próbkowanie na GPU.
Stan > 2 GiB shardowany po buforach (`binding_array`, do 8); stan > VRAM liczony blokowo
z wymianą do RAM (tryb wolny, jawnie sygnalizowany). Backend natywny (Vulkan/Metal/DX12)
i WebGPU w przeglądarce z jednego źródła WGSL; różnice limitów czytane z `Adapter::limits()`,
nie zakładane. Wynik musi być zgodny z CPU `complex64` do `1e-5` na zestawie złotym; f64 na
GPU nie istnieje i UI mówi to przy wyborze targetu.

### 6.4 Moduł Pythona `tentaquant_sim` (pyo3 + maturin)

Pierwszy pyo3 w repo. Crate `tentaflow-quantum-py` (cienki, w tym samym katalogu co crate),
`maturin build --release` w `release.yml` per platforma (linux x86_64/aarch64, macos aarch64,
windows x86_64; abi3 ≥ 3.11, żeby jeden wheel obsłużył 3.11–3.13 z python-build-standalone).
Jeden wheel per platforma zawiera WSZYSTKIE backendy (PTX + WGSL to kilka MB tekstu; wgpu i
cudarc linkują się statycznie, sterowniki ładowane w runtime), więc `[[install_variants]]` nie
rozgałęzia się per producent GPU — tak samo jak jedna binarka `tentaflow` obsługuje CUDA
i Vulkan. Wektor stanu wraca do Pythona zero-copy jako `numpy.ndarray` (`numpy` crate, bufor
własnością obiektu Rust), obwód wchodzi jako OQ3 (`qiskit.qasm3.dumps`) albo jako lista bramek
z `BackendV2` bez round-tripu przez tekst.
API: `Simulator(device="cpu"|"gpu", precision="single"|"double")`, `run(qasm3, shots)`,
`statevector(qasm3)`, `expectation(qasm3, paulis)`, `devices()`. SDK `tentaquant` (czysty
Python, w bundle'u) opakowuje go w `BackendV2` z `Target` ze wszystkich bramek `stdgates.inc`
i wystawia `describe()` z nazwą backendu (`cpu`/`cuda`/`wgpu`) i precyzją. Wheel trafia do bundle'a przez `local_wheels`
(§2.5, luka 4). Bez pyo3 alternatywą jest wołanie symulatora w Core przez kanał zwrotny — decyzja
§18.

### 6.5 Czego nie budujemy

Symulacji szumu (jest w Aer/QDK na T2), symulacji pulsów, tensor networks, wielo-GPU. Każda
z tych rzeczy ma w ekosystemie dojrzałą implementację o rząd wielkości lepszą niż to, co
powstałoby tu; crate ma być **szybki dla ≤28 q na CPU, ≤32 q na GPU i identyczny w
przeglądarce**.

---

## 7. Broker QPU

### 7.1 Kontrakt

```rust
#[async_trait]
pub trait QpuProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;                       // v1: Ibm; IonQ/Iqm/Qrmi(ResourceType) w §7.4
    async fn backends(&self, creds: &Creds) -> Result<Vec<BackendInfo>>; // n_qubits, coupling, basis, queue, status, calibration_at
    async fn estimate(&self, req: &SubmitRequest) -> Result<CostEstimate>; // seconds / credits / shots, currency, confidence
    async fn submit(&self, creds: &Creds, req: &SubmitRequest) -> Result<ProviderJob>;
    async fn status(&self, creds: &Creds, job: &ProviderJob) -> Result<JobStatus>;
    async fn result(&self, creds: &Creds, job: &ProviderJob) -> Result<JobResult>; // counts / quasi-dists / expectation + raw
    async fn cancel(&self, creds: &Creds, job: &ProviderJob) -> Result<()>;
}
```

Nazwy i podział odpowiadają `QuantumResource` z QRMI (`acquire/release` mapują się na
`Session`/`Batch` u IBM i są **wewnątrz** `submit`, bo plan Open ich nie ma). Jedna implementacja
= jeden plik w `tentaquant/providers/`.

### 7.2 Dostawca v1: IBM

- **IBM** (`providers/ibm.rs`, natywny REST): IAM token z API key (cache ≤55 min), `Service-CRN`,
  `IBM-API-Version`; `GET /backends` + `/backends/{id}/properties` (kalibracja), `POST /jobs`
  z `program_id = sampler|estimator`, wejście OQ3 ISA, tryb `job` (Open) lub `batch`; sondowanie
  `GET /jobs/{id}` z backoffem 5→60 s; wynik `GET /jobs/{id}/results`. Kosztorys: szacowany czas
  QPU z `shots × depth` i historii instancji (IBM raportuje `usage.seconds` po zakończeniu — wtedy
  korekta limitu). **Transpilacja do ISA w usłudze Python** (`generate_preset_pass_manager` na
  `Target` pobranym przez Core): Core dostaje gotowy OQ3 + `program_id` + `pubs`.
- Konta: jedno żądanie `submit` niesie `creds` **jednego** konta — osobistego użytkownika
  (domyślnie, gdy ma token) albo konta laboratorium (gdy nie ma własnego, lub gdy jawnie wybrał
  je w kosztorysie). `usage.seconds` po zakończeniu koryguje pulę tylko dla konta laboratorium.
- Wspólne (obowiązują każdego przyszłego dostawcę): `retry` tylko na 429/5xx z `Retry-After`, nigdy na `submit` bez idempotency (IBM nie
  daje klucza idempotencji → po timeout `submit` = stan `unknown` + sonda po `client_job_tag`,
  nie ponowne wysłanie — ten sam wzorzec co `git_commit` w Code Studio §11.5).

### 7.3 Poświadczenia, limity, zgody, koszt

- Tabela `providers` (§9.2) z `secret_enc` (`SettingsCipher`), odszyfrowanie **w jednym miejscu**
  (`providers::creds_for(job)`), nigdy w logach, nigdy w `ProviderJob`; listing zwraca
  `<redacted>` (jak `Settings → Dostępy zewnętrzne`).
- Dwa zakresy. `scope = user`: **konto osobiste**, domyślna droga na QPU — użytkownik wpisuje
  API key + CRN w „Moje konto IBM” (Q12 dla każdego z `quant.run.qpu`), widzi tylko on, run idzie
  z jego planu (Open: 10 min/28 dni po stronie IBM), bez puli i bez zgody, audyt tak.
  `scope = instance`: **konto laboratorium**, najwyżej jedno, zakłada `quant.admin`; używane, gdy
  użytkownik nie ma własnego tokenu albo wybrał je jawnie w kosztorysie.
- Pula konta laboratorium (`qpu_budget`): sekundy QPU na okres (domyślnie 28 dni, jak plan IBM)
  dla całej instancji + domyślny limit sekund per osoba; opiekun może podnieść limit konkretnej
  osobie (`qpu_user_limits`). Zużycie rezerwowane przy `submit` (`reserved`), rozliczane po
  `result` (`used` z `usage.seconds`), zwalniane po `cancel/failed`. Rezerwacja to jeden warunkowy
  `UPDATE … WHERE reserved + used + ? <= budget` na puli i drugi na limicie osoby — nie da się
  przekroczyć żadnego z nich równolegle.
- Kosztorys przed wysłaniem: dialog z liczbą shotów, głębokością po transpilacji, szacowanymi
  sekundami, kolejką backendu i **z którego konta** idzie run; przy koncie laboratorium także
  pozostała pula i limit osoby. Run z konta laboratorium zawsze czeka na zgodę opiekuna
  (`approvals`): karta „Do zatwierdzenia” na Pulpicie (tylko `quant.instruct`) + powiadomienie
  Core; szczegół zgody to Q13 w trybie odczytu z kosztorysem.
- Audyt: każda decyzja (submit, approve, deny, cancel) do `audit_log` z ids i liczbami, bez
  treści obwodu.

### 7.4 Dostawcy v2 (po F5)

IonQ (REST v0.4, `ionq.circuit.v1` JSON z własnego generatora), IQM Resonance (przez QRMI
`iqm-server` za feature `quantum-qrmi` — spike C przenosi się tutaj), Braket, Azure Quantum,
Quantinuum (QIR z `qbraid-qir`/`pyqir` w usłudze), Pasqal (QRMI), qBraid.
Wspólny problem: ich SDK-i żyją w Pythonie i wymagają poświadczeń w procesie. Rozwiązanie
(decyzja F5): Core uruchamia **adapter dostawcy jako osobną, krótkotrwałą sesję jądra z
`tier = broker`** (nie sandbox użytkownika), z poświadczeniami wstrzykniętymi w env na czas jednego
zadania i siecią ograniczoną do endpointów dostawcy — model „adapter po stronie Core” z Code Studio
§17.3, tylko w Pythonie. Nigdy współdzielony z sesją użytkownika.

---

## 8. Threat model i izolacja

### 8.1 Aktorzy

Użytkownik (niezaufany kod, może próbować wyciągnąć poświadczenia, wyczerpać pulę, wyjść
w sieć, zjeść GPU), opiekun (zaufany w laboratorium, nie w organizacji), admin org, usługa
`quantum-*` (półzaufana: wykonuje, nie decyduje), dostawca chmurowy (zewnętrzny; odpowiedzi to
dane, nie instrukcje), inny węzeł mesh (zaufany po parowaniu, jak wszędzie).

### 8.2 Granice

- Sandbox jądra (`SandboxLimits::quantum_kernel`): rootfs RO, tmpfs `/work` (limit 2 GiB
  domyślnie), `cap_drop ALL`, `no-new-privileges`, PID 256, RAM per tier (T2 8 GiB, T3 = VRAM +
  16 GiB), CPU 2–4, `network_mode = none` **plus** gniazdo do brokera usługi (unix socket
  montowany do kontenera) — kod użytkownika nie ma trasy do internetu; jedyne wyjście to broker
  z `session_token`. Dla T3 `--gpus device=<idx>`; jedno jądro = jedno GPU (bez współdzielenia
  VRAM w v1).
- `trusted_native` (python-bundle bez kontenera): **brak izolacji od hosta** — dozwolony bez
  pytania tylko, gdy matryca instancji daje `quant.run` jednej osobie; przy większej liczbie
  `quant.admin` musi potwierdzić jawnie w Q14/Q12 (ostrzeżenie z listą osób), UI oznacza tryb
  trwale jak Code Studio §19, a każda nowa osoba z `quant.run` w matrycy ponawia ostrzeżenie
  na Pulpicie admina. Bez potwierdzenia i bez runtime kontenerów instancja nie ma T2. Limity czasu/RAM egzekwowane przez
  `resource`/`ulimit` w procesie jądra — tylko tyle.
- Poświadczenia: wyłącznie Core (§7.3). Usługa i jądro nie mają do nich ścieżki nawet w
  `trusted_native`.
- Wyjścia komórek to dane użytkownika: `text/html` z jądra renderowane **po sanityzacji**
  (allowlista tagów, bez skryptów, bez `style` z URL) w `tf-mime-output`; `image/svg+xml`
  rasteryzowane albo odrzucone (SVG niesie skrypty). Markdown komórek renderowany istniejącym
  rendererem dashboardu.
- Obwody i dane z dostawców (kalibracje, wyniki) walidowane schematem przed zapisem; nazwa backendu
  z odpowiedzi nigdy nie staje się ścieżką ani kluczem bez walidacji `[a-z0-9_]`.
- Przykłady i katy wbudowane są kodem z repo (zaufane); przykłady dodane przez opiekuna to kod
  laboratorium — uruchamiane w sandboxie jak każdy inny.

### 8.3 Czego świadomie nie chronimy

Przed użytkownikiem, który celowo wyczerpuje własny limit z puli; przed side-channelami między jądrami na tym
samym GPU (dlatego jedno jądro = jedno GPU); przed dostawcą, który zwraca błędne wyniki; przed
złośliwym opiekunem wobec własnego laboratorium (widzi runy i postępy osób — to cel, nie luka);
w trybie `trusted_native` — przed czymkolwiek poza przypadkowym błędem.

---

## 9. Model danych

### 9.1 Baza główna (replikowana)

Tylko to, co platforma już trzyma: wiersz `addons` instancji, `addon_packages`,
`addon_permission_defaults`, matryca uprawnień, `__node_status/<node_id>` w `addon_config`,
`audit_log`. **Żadnej nowej tabeli TentaQuant w bazie głównej** — członkostwo laboratorium jest
w bazie instancji, bo instancja i tak jest globalna dla floty, a jej katalog replikuje się przez
sync jak katalog innych aplikacji.

### 9.2 `tentaquant.db` — per instancja

| Tabela | Klucze / uwagi |
|---|---|
| `user_settings(user_id PK, settings_json, updated_at)` | preferencje per członek (domyślny tier, węzeł); **nie** członkostwo — to matryca (§10) |
| `projects(id, owner_user_id, name, description, visibility, created_at, archived_at, linked_project_id)` | `visibility`: `private/lab`; `linked_project_id` → Project Studio; właściciel = twórca, zmiana właściciela tylko przez „przekaż projekt” |
| `project_shares(project_id, user_id, role, granted_by, granted_at)` | `role`: `editor/viewer` — wzór `ml_project_members` z ML Studio; wiersz jest martwy, gdy matryca nie daje `user_id` `quant.read` (UI pokazuje „bez dostępu do laboratorium” i link do Addons). Lista widoczna dla użytkownika = własne ∪ udostępnione mu ∪ `visibility = lab`; opiekun (`quant.instruct`) widzi metadane runów wszystkich, ale **nie** treść cudzych prywatnych projektów; `viewer` uruchamia tylko T0 bez zapisu |
| `files(id, project_id, path, kind, sha256, size, updated_at)` | `kind`: `notebook/py/qasm/data/md`; treść w CAS |
| `notebooks(id, project_id, file_id, current_version, updated_by)` | |
| `notebook_versions(notebook_id, version, cells_json, sha256, author, created_at)` | append-only; blokowanie optymistyczne jak `test_case_versions` |
| `cell_outputs(run_id, cell_id, seq, mime_json, artifact_sha256)` | mime bundle; duże dane w CAS |
| `runs(id, project_id, notebook_id, cell_id, kind, target, node_id, status, started_at, ended_at, error, metrics_json, user_id, pinned_at, thumbnail_sha256, keyframes_sha256)` | `kind`: `cell/circuit/program/kata/flow`; `metrics_json`: czas, kubity, shoty, pamięć; `pinned_at` = galeria „Wyniki”; miniatura i klatki kluczowe w CAS |
| `qpu_jobs(run_id PK, provider_id, backend, provider_job_id, client_tag, status, estimate_json, usage_json, cost_json, submitted_at, ended_at)` | `client_tag` do sondy po timeout |
| `providers(id, scope, user_id, kind, label, region, config_json, secret_enc, created_by, created_at, disabled_at)` | `scope`: `user` (własny token, `user_id` = właściciel, listing tylko dla niego) / `instance` (konto laboratorium, `UNIQUE` na `scope = instance`, `user_id` NULL); `secret_enc` szyfr |
| `qpu_budget(provider_id PK, period, pool_seconds, default_user_seconds, updated_by, updated_at)` | pula konta laboratorium; `period`: `28d` domyślnie |
| `qpu_user_limits(provider_id, user_id, seconds, set_by, set_at)` | nadpisanie limitu osoby przez opiekuna; brak wiersza = `default_user_seconds` |
| `quota_ledger(id, provider_id, user_id, run_id, reserved, used, period_key, at)` | rozliczenie puli; sumy per (provider, period) i per (provider, user, period) |
| `approvals(id, run_id, requested_by, decided_by, decision, reason, requested_at, decided_at, expires_at)` | |
| `examples(id, source, title, tags_json, variants_json, readme_md, circuit_qasm, created_by, created_at)` | `source`: `bundled/instance`; wbudowane seedowane przy `init`, aktualizowane po hashu |
| `kata_progress(user_id, kata_id, status, attempts, best_score, points, last_run_id, updated_at)` | ranking i seria liczone z tej tabeli (`points`, daty `updated_at`), bez osobnej tabeli |
| `kernel_sessions(id, user_id, notebook_id, node_id, tier, service_name, state, started_at, last_activity_at, limits_json)` | **kopia informacyjna**; źródłem prawdy jest rejestr runtime na węźle (§9.3) |
| `settings(key, value_json)` | `max_qubits_*`, TTL, domyślny tier, tryb izolacji, `ranking_enabled` (domyślnie `true`), `trusted_native_ack` (kto i kiedy potwierdził) |
| `app_schema_version` | przez `run_versioned_migrations` |

### 9.3 Node-local runtime (nie replikowane)

`<data>/tentaquant/<addon_id>/runtime.db` na węźle z usługą: `kernel_sessions` (źródło prawdy,
`session_token_hash`, PID/kontener, TTL), `local_runs` (znacznik `register_local_run`), cache
`backend_targets` (Target IBM do transpilacji, `fetched_at`, TTL 6 h). Wzór: `code_studio/paths.rs`
+ `workspace_db.rs`.

### 9.4 CAS i układ katalogów

```
<orgs>/<org>/addons/tentaquant-<8hex>/
  tentaquant.db                       # §9.2
  files/<sha256>                      # treść plików, artefakty wyjść (png, json, wektory stanu)
  exports/<run_id>.ipynb              # generowane na żądanie, GC po 24 h
<data>/tentaquant/tentaquant-<8hex>/  # node-local
  runtime.db
  work/<session_id>/                  # katalog roboczy jądra (tmpfs w kontenerze; tu w trybie natywnym)
```

Retencja: artefakty runów starsze niż `retention_days` (domyślnie 180) i nieprzypięte → GC;
wektory stanu > 64 MiB nigdy nie są zapisywane (tylko próbki/statystyki) — zapis ma jawny limit
w kosztorysie runu.

---

## 10. Uprawnienia

### 10.1 Zasada: platforma decyduje, aplikacja tylko pyta

Jedynym źródłem „kto może co” w laboratorium jest matryca uprawnień jego instancji
(`addon_permissions` + `addon_permission_defaults`, kluczowane `addon_id` instancji), edytowana
przez administratora w **Addons → instancja → Uprawnienia** (per grupa / per użytkownik / default)
i **Widoczność** (które grupy widzą kafelek, `admin_only`). TentaQuant:

- deklaruje katalog w `[[permission]]` manifestu i **nic więcej** — bez migracji ról org, bez
  tabeli członków, bez własnego ekranu zarządzania członkami;
- każdy handler woła `require_instance_permission(ctx, "tentaquant", instance_id, <perm>)`
  (§2.2) i dostaje odpowiedź z hierarchii admin > użytkownik > grupa > default > deny;
- „zaproszenie do laboratorium” = administrator (albo osoba z `quant.admin`, która i tak jest
  adminem org) nadaje grupie `quant.read` + `quant.run` w matrycy instancji; usunięcie
  z grupy org cofa dostęp we wszystkich laboratoriach tej grupy naraz;
- listy „osób z dostępem” w UI (tablica postępu, filtr runów opiekuna) są **odczytem** matrycy:
  użytkownicy z przyznanym `quant.read` w tej instancji (ekspansja grup jak w
  `PermissionChecker::resolve_permissions`), nigdy osobnym stanem;
- limity puli (§7.3) adresują użytkowników z matrycy (domyślny limit + nadpisania per osoba),
  więc lista osób w Ustawieniach to również odczyt matrycy.

### 10.2 Manifest (`src/tentaquant/app-manifest.toml`)

| id | risk | default | Znaczenie |
|---|---|---|---|
| `quant.read` | low | allow | Wejście do laboratorium; projekty `lab`, przykłady, katy, własny postęp. To jest „bycie członkiem” |
| `quant.run` | low | allow | Własne projekty i notatniki, uruchamianie T0–T3 w ramach limitów instancji, fork przykładów |
| `quant.run.gpu` | low | allow | Targety T3 (GPU węzłów) i `tentaquant.backend("gpu")`; kata 20 |
| `quant.run.qpu` | medium | allow | Wysyłanie na QPU: z własnego tokenu IBM bez dalszych bramek; z konta laboratorium — pula, limit osoby i zgoda opiekuna |
| `quant.instruct` | medium | deny | Opiekun: metadane runów i postęp kursu wszystkich osób, zatwierdzanie zgód, limity per osoba, publikowanie projektu jako `lab`, przykłady laboratorium, wyłączenie rankingu |
| `quant.admin` | critical | deny | Konto laboratorium IBM i pula, tryb izolacji, węzły, `max_qubits`, retencja, usunięcie instancji. Spełnione tylko przez admin-bypass (rola `admin`/`is_admin`/grupa `admins`) — jawny `allow` w matrycy jest honorowany przez checker, ale UI oznacza to uprawnienie jako „tylko admin” i nie przewiduje nadawania go zwykłej grupie |

`default = "allow"` dla `read` i `run` oznacza: każda grupa, której **Widoczność** pokazuje
kafelek, może wejść i liczyć; instancja tworzona dla jednej grupy dostaje w Widoczności tylko ją.
`run.qpu` jest `allow`, bo run z własnego tokenu kosztuje laboratorium zero, a run z konta
laboratorium i tak przechodzi przez pulę i zgodę; admin może je odebrać grupie jednym `deny`.

### 10.3 Macierz zdolności (dla czytelności — to nie są byty w kodzie)

| Zdolność | `read` | `run` | `run.gpu` | `run.qpu` | `instruct` | `admin` |
|---|---|---|---|---|---|---|
| Czytać projekty `lab`, przykłady, kurs; T0 w przeglądarce bez zapisu | ✓ | | | | | |
| Własne projekty, notatniki, runy T0–T2, kurs z oceną i punktami | | ✓ | | | | |
| Targety T3 (GPU węzłów), `backend("gpu")`, kata 20 | | ✓ | ✓ | | | |
| QPU z własnego tokenu; z konta laboratorium w puli i za zgodą | | ✓ | | ✓ | | |
| Metadane runów i postęp wszystkich; zgody; limity osób; ranking on/off | | | | | ✓ | |
| Publikować projekt jako `lab`, dodawać przykłady laboratorium | | | | | ✓ | |
| Konto laboratorium IBM i pula, izolacja, węzły, `max_qubits`, usunięcie | | | | | | ✓ |

Typowe zestawy: *użytkownik* = `read` + `run` + `run.gpu` + `run.qpu` (domyślne `allow`),
*opiekun* = to samo + `instruct`, *obserwator* = `read`; w laboratorium jednoosobowym admin ma
wszystko z bypassu. Nazwy zestawów istnieją tylko w dokumentacji i i18n opisów uprawnień, nie
w bazie. Projekt udostępniony jako `viewer` daje mniej niż `run`: uruchamianie wyłącznie T0
w przeglądarce, bez zapisu wyniku do projektu i bez wiersza w `runs`.

Nieuprawniony (brak `quant.read` w tej instancji) dostaje jednolite `AppUnavailable`/`NotFound`
z bramki (bez ujawniania istnienia laboratorium). Notatniki **prywatne** nie mają obejścia:
`instruct` widzi notatniki `lab` i wszystkie *runy* (metadane, metryki, zużycie), a admin org
widzi liczby, nie treść; wejście w treść cudzego prywatnego notatnika nie istnieje jako
operacja — właściciel musi go opublikować jako `lab` (wzór prywatności czatów w Projektach
i sesji w Code Studio, `dispatch/code_studio.rs:5-11`).

### 10.4 Co robi platforma, a co aplikacja przy usunięciu

Odinstalowanie instancji (Addons) wywołuje `hooks.teardown_plan`/`teardown`: aplikacja zamyka
pulę, anuluje żywe sesje jądra na węzłach, próbuje `cancel` na jobach QPU w toku (i raportuje
w planie, ile ich jest — admin widzi to przed potwierdzeniem, jak w TentaNas), wymazuje katalog.
Wpisy matrycy usuwa platforma razem z wierszem `addons`.

---

## 11. Protokół i mesh

### 11.1 `TentaQuantPayload`

`tentaflow-protocol/src/tentaquant.rs`, `MessageBody::TentaQuantBody`, warianty
żądanie/odpowiedź w jednym enumie, **append-only, nigdy rename** (tagi po nazwie), pola nowe z
`#[serde(default)]`. Każde żądanie niesie `instance_id: String`. Rodziny:

- `Lab*`: `LabListRequest/Response` (instancje pakietu, w których matryca daje wołającemu
  `quant.read`, + stan węzłów), `LabOverview`, `LabPeople` (odczyt matrycy: użytkownicy z
  `quant.read`, z zestawem przyznanych uprawnień — tylko dla `instruct`), `SettingsGet/Set`.
  **Brak** wariantów do nadawania uprawnień — to robi istniejący `AddonPermission*` z Addons.
- `Project*`, `File*` (upload w 4 MiB porcjach jak Project Studio), `Notebook*`
  (`Get`, `Save{expected_version}`, `Versions`).
- `Kernel*`: `SessionStart{notebook_id, tier, node_id}`, `SessionStop`, `Execute{session_id,
  cell_id, code}`, `Interrupt`, `Subscribe{session_id}` → strumień `KernelEvent{seq, msg}`
  (mime bundle Jupytera).
- `Run*`: `List`, `Get`, `Cancel`, `Artifact{run_id, sha256}` (signed URL scope
  `TentaQuantArtifact`, jak `ProjectStudioExport`), `Subscribe{run_id, after_seq}` → strumień
  `RunEvent{seq, kind: output|state_keyframe|state_frame|metrics|done}`, `Keyframes{run_id}`
  (z CAS po zakończeniu), `StateQuery{run_id, pairs|top_k}` (na żądanie z żywej sesji),
  `LiveScrub{run_id, step, t}` (tryb „na żywo” T3), `Pin{run_id, pinned}`, `Compare{run_ids}`
  (metryki + serie), `Export{run_id, parts}` → artefakt `.zip` przez `Artifact` (§13.6).
- `Circuit*`: `Validate{qasm3}` (IR + błędy z liniami), `Simulate{qasm3, opts}` (T1), `Export{
  qasm3, format}`, `Transpile{qasm3, backend}` (przez usługę).
- `Target*`: `List` (tiery, węzły, GPU, backendy QPU z kolejką i kalibracją, konto i pozostała
  pula), `Resolve{auto, circuit_meta}` (wynik reguły §5.3 do pokazania przed startem).
- `Provider*` (`MyAccountSet/Get/Test`, `LabAccountSet/Get`), `Budget*` (`Get`, `Set`,
  `UserLimitSet`), `Approval*` (`Request`, `Decide`, `ListPending`).
- `Qpu*`: `Estimate`, `Submit{run_id, backend, shots, ...}`, `JobStatus`, `JobResult`.
- `Example*`: `List`, `Get`, `Fork{example_id, variant, project_id}`, `Create` (instancja).
- `Kata*`: `List`, `Get`, `Submit{kata_id, qasm3|code}` → ocena T1, `Progress`, `Ranking`
  (pusty, gdy `ranking_enabled = false`).
- `Flow*`: nic — bloki flow używają wewnętrznych API Core, nie protokołu.

Kodek: `tentaflow-protocol-wasm/src/lib.rs` → `www/js/protocol/codec.js` (`tentaQuant*`),
wywołania przez `ApiBinary.one/list` z `{ targetNodeId }` jak `nas()`/`nasOn()`
(`www/js/modules/tentanas.js:184-197`).

### 11.2 Strumienie

`KernelEvent` i `RunEvent` (wyjścia, `state_keyframe`, `state_frame` z trybu „na żywo” T3,
metryki) to strumienie z `seq` i buforem odtworzenia (512 ramek) po stronie węzła
wykonującego; `Subscribe{after_seq}` wznawia po utracie połączenia dashboardu. Wyjścia większe niż
budżet inline (4 MiB) idą do CAS, a strumień niesie referencję — dokładnie kontrakt
`mesh_stream.rs`.

### 11.3 Mesh

- Żądania unarne: `AppRouteOp` z `targetNodeId` (węzeł usługi). Bez nowych wariantów
  `MeshCommandType` dla unarnych.
- Strumienie: `app_route` ich nie forwarduje. **Decyzja: uogólnienie** `CodeStudioStreamOpen/
  Pull/Result` do `AppStreamOpen{app: package_id, ...}` — jeden relay z kluczem (aplikacja, sesja,
  strumień, węzeł konsumenta, użytkownik), Code Studio przepięte na niego w tej samej zmianie
  (reguła 2 i 3: bez równoległej kopii, bez `_v2`). To jest jedyna zmiana platformowa z ryzykiem
  regresji w Code Studio — ma własny krok w F3 z testami obu aplikacji.
- Usługa `quantum-*` na węźle zdalnym rozmawia z **lokalnym** Core tego węzła (kanał zwrotny jest
  lokalny); dashboard nigdy nie łączy się z usługą bezpośrednio (pamięć projektu: usługi
  proxowane przez komendy, endpoint na loopbacku).

---

## 12. Przykłady i kurs

### 12.1 Format przykładu

```
tentaflow-core/src/tentaquant/examples/<id>/
  example.toml      # id, title_{pl,en}, level (intro/core/advanced), tags, qubits, variants = [cpu, gpu, qpu], est_qpu_seconds
  README.md         # teoria w 1–2 ekranach, co porównać między wariantami
  circuit.qasm      # obwód kanoniczny (OQ3) — otwiera się w Studio obwodów
  cpu.py            # wariant T2 (Qiskit/Aer lub PennyLane)
  gpu.py            # wariant T3/T2-wgpu (`tentaquant.backend("gpu")`) — ten sam algorytm, inny target i skala
  qpu.py            # wariant T4 przez `tq.run(target="qpu:...")` z porównaniem do symulacji
  classical_cpu.py  # (opcjonalnie) ten sam problem klasycznie, NumPy
  classical_gpu.py  # (opcjonalnie) klasycznie na GPU: CuPy / Numba / PyTorch — z adnotacją, na którym sprzęcie działa
  expected.json     # wynik referencyjny (counts/energia) do testu CI na T1/T2
```

Wbudowane przez `include_dir!` (jak addony w `build.rs`), seedowane do `examples` z hashem
zawartości; zmiana w repo aktualizuje wpisy `source = bundled`, nigdy nie nadpisuje forków.

### 12.2 Lista startowa (v1)

| # | Przykład | Kubity | cpu | gpu | qpu | klasycznie CPU/GPU | Czego uczy |
|---|---|---|---|---|---|---|---|
| 1 | Stan Bella i korelacje | 2 | ✓ | — | ✓ | — | superpozycja, splątanie, shoty vs teoria; **pierwszy run na QPU** |
| 2 | GHZ n-kubitowy | 3–28 | ✓ | ✓ | ✓ | — | skalowanie 2^n: to samo na 10 q (cpu), 28 q (gpu), 5 q (qpu z szumem) |
| 3 | Teleportacja z obwodem dynamicznym | 3 | ✓ | — | ✓ | — | `if` na bitach, MCM, ograniczenia IBM (`if_test`) |
| 4 | QFT i estymacja fazy | 4–24 | ✓ | ✓ | ✓ | ✓ (FFT / diagonalizacja NumPy, CuPy) | struktura, głębokość po transpilacji, gdzie szum zabija wynik; QFT vs FFT |
| 5 | Grover 2–4 q | 2–4 | ✓ | — | ✓ | ✓ (przeszukiwanie siłowe: pętla NumPy vs kernel Numba/CuPy) | wyrocznia, amplifikacja; √N zapytań vs N na GPU — i dlaczego przy 4 q GPU i tak wygrywa |
| 6 | QAOA MaxCut (hybryda) | 6–20 | ✓ | ✓ | ✓ | ✓ (heurystyka Goemans–Williamson / wyżarzanie na GPU) | pętla wariacyjna; optymalizator CPU, ewaluacja GPU, finał QPU; jakość cięcia vs klasyka |
| 7 | VQE H₂ (hybryda) | 2–4 | ✓ | ✓ | ✓ | ✓ (dokładna diagonalizacja, PyTorch) | Hamiltonian Pauliego, `estimate`, energia vs FCI |
| 8 | Kod powtórzeniowy / mały kod powierzchniowy | 5–17 | ✓ (Stim przez Aer/qdk) | ✓ (`stim` target CUDA-Q) | — | — | QEC, syndromy, dlaczego stabilizer symuluje tysiące kubitów |
| 9 | Losowy obwód — benchmark tierów | 20–32 | ✓ | ✓ | — | — | ten sam obwód, pomiar czasu i pamięci per tier i per GPU (CUDA vs wgpu); wykres |
| 10 | Klasyfikator kwantowy (QML, PennyLane) | 4 | ✓ | ✓ | — | ✓ (ta sama sieć klasycznie, PyTorch) | gradienty, `lightning.gpu`, gdzie GPU pomaga a gdzie nie |
| 11 | Próbkowanie / Monte Carlo — szacowanie amplitudy | 3–10 | ✓ | ✓ | ✓ | ✓ (Monte Carlo na GPU, CuPy) | kwadratowe przyspieszenie estymacji vs 1/√N klasycznie |

Każdy wariant to **kompletny, uruchamialny plik** — nie szkielet. `expected.json` porównywany
w CI na T1 (obwody) i w teście integracyjnym `#[ignore]` z usługą (cpu.py).

### 12.3 Katy

Format zbliżony do Quantum Katas (MIT), ale w naszym IR: `kata.toml` (id, tytuły, grupa,
kolejność, punkty, wymagany tier), `task.md`, `task.qasm` (szkielet z `// TODO` w treści zadania
— to treść dla użytkownika, nie kod produkcyjny), `verify.toml` (rodzaj oceny: `state_equals` /
`unitary_equals` / `counts_tvd_below` / `python_test`), `solution.qasm`. Ocena T1 w Core
(`grade`), deterministyczna, < 100 ms; katy Pythonowe (`python_test`) oceniane w sesji jądra
pytestem jak w test-runnerze.

Kurs to **24 zadania w jednej kolejności** od najprostszego do najtrudniejszego, w czterech
grupach (bramki i Bloch → superpozycja i splątanie → algorytmy → szum i mitigacja); grupa
odblokowuje się po poprzedniej, **bez terminów, przydziałów i ocen** — kurs jest dla każdego
użytkownika, nie dla klasy. Wymagany tier: 1–19 T0/T1, kata 20 (losowy obwód 24 q) wymaga
`quant.run.gpu`, katy 21–24 (szum, Bell na `ibm_torino`, TREX, ZNE) zaliczają się na symulatorze
z modelem szumu z kalibracji IBM (T1 `sim::noise` lub T2 Aer) — prawdziwy QPU to w nich
przycisk „sprawdź naprawdę” z własnego konta, opcjonalny i nigdy niewymagany do zaliczenia.
Punkty per kata, seria dni z `kata_progress.updated_at`, ranking laboratorium (top 5 + własna
pozycja, z nazwiskami — decyzja §18.20) włączony domyślnie, opiekun wyłącza go w Ustawieniach.
Opiekun widzi tablicę postępu wszystkich osób.

### 12.4 Asystent

Modele `Qiskit/granite-3.3-8b-qiskit` (GGUF) i `Qwen2.5-Coder-14B-Qiskit` jako **zwykłe aliasy**
llama.cpp/vLLM; TentaQuant nie ma własnego chatu — komórka notatnika ma akcję „wyjaśnij / popraw”
wołającą istniejący flow czatu z kontekstem (kod komórki, błąd, obwód w OQ3) przez
`AiGateway`. Wyniki idą do audytu AI jak każda inna rozmowa. Bez wysyłania kodu poza organizację,
chyba że alias wskazuje dostawcę chmurowego — wtedy obowiązuje polityka aliasu, nie TentaQuant.

---

## 13. UI

### 13.1 Ekrany (`www/js/modules/tentaquant.js` + `tentaquant/*.js`)

1. **Lista laboratoriów** (gdy matryca daje użytkownikowi `quant.read` w > 1 instancji lub jest
   adminem): tabela z dwuwierszowymi komórkami (nazwa + id), liczba osób z dostępem, ostatnia
   aktywność, zużycie QPU; akcja „Nowe laboratorium” (admin) = kreator instalacji instancji
   z `display_name`, a zaraz po nim skok do Addons → instancja → Widoczność/Uprawnienia, bo bez
   nadania grupie dostępu laboratorium jest puste.
2. **Pulpit laboratorium**: KPI (`tf-stat-card` w siatce): osoby z dostępem, runy 7 dni, sekundy
   QPU pozostałe w puli konta laboratorium (albo status własnego tokenu), węzły z GPU online;
   ostatnie runy; karta **„Do zatwierdzenia (N)”** tylko dla `quant.instruct` (lista próśb o run
   z konta laboratorium, klik = Q13 w trybie odczytu z kosztorysem, zatwierdź/odrzuć) — to samo
   idzie do dzwonka powiadomień Core.
3. **Projekty**: karty jak w ML Studio w trzech sekcjach — „Moje projekty” (właściciel; ikona
   udostępniania na karcie), „Udostępnione mi” (chip roli Edytor/Przeglądający i właściciel),
   „Materiały laboratorium” (`visibility = lab`, tylko odczyt, dla opiekuna edycja) — plus karta
   „+ Nowy projekt” (okno: nazwa, opis, start pusty / z przykładu / z szablonu; prywatny
   domyślnie). Okno „Udostępnij”: lista osób z rolą, dodanie po nazwie użytkownika, przełącznik
   „całe laboratorium może oglądać”, ostrzeżenie przy osobie bez `quant.read`. W projekcie:
   drzewo plików (`tf-tree`), notatniki, upload (`tf-file-input`), fork przykładu; zakładka
   **Wyniki** projektu (galeria miniatur runów, przypinanie, porównanie — §13.6).
4. **Notatnik**: kolumna komórek; komórka `code` = `tf-code-editor` (python), `markdown` = edytor +
   podgląd, `circuit` = `tf-quantum-circuit` z zakładką tekstową (OQ3, tokenizer `openqasm`);
   wyjścia w `tf-mime-output`; pasek: target (`tf-select` z grupami tier/węzeł/backend), stan sesji
   jądra (`tf-status-pill`), Run / Run all / Interrupt; prawy panel: żywy stan (Bloch per kubit,
   histogram, amplitudy) dla ostatniej komórki `circuit`.
5. **Studio obwodów**: pełnoekranowy `tf-quantum-circuit` (paleta bramek, drag-drop, parametry,
   bramki własne), T0 przy każdej zmianie, tryb krokowy (suwak po bramkach), panel stanu, eksport
   (OQ3 / Qiskit / SVG), „uruchom na…” z kosztorysem dla QPU.
6. **Runy**: tabela z filtrami (tier, stan, użytkownik dla opiekuna), szczegół runu z
   artefaktami i metrykami; przycisk „Otwórz wynik” prowadzi do **pełnoekranowego widoku runu**
   (Ewolucja / Stan / Histogram / Porównanie / Dane i eksport — §13.6).
7. **Urządzenia**: tiery i węzły (GPU z VRAM z heartbeatu), backendy QPU z liczbą kubitów, mapą
   sprzężeń (`tf-relation-graph`), kolejką, datą kalibracji i błędami bramek (`tf-heatmap`),
   z którego konta pójdzie run (własny token / konto laboratorium z pozostałą pulą).
8. **Przykłady**: galeria kart (`tf-choice-card`) z filtrami poziom/tag/wariant; widok przykładu:
   README, trzy zakładki cpu/gpu/qpu z kodem, obwód, przycisk „Fork do projektu”.
9. **Kurs**: 24 zadania w grupach z postępem (`tf-progress-bar`), widok bieżącego zadania:
   treść, edytor obwodu lub kodu, „Sprawdź” (T1), wynik z podpowiedzią, „sprawdź naprawdę” na QPU
   w katach 22–24; ranking (jeśli włączony) i tablica postępu wszystkich osób dla opiekuna.
10. **Ustawienia laboratorium**: „Moje konto IBM” (każdy z `quant.run.qpu`: API key + CRN jako
    `is_secret`, test połączenia, plan i pozostałe minuty z IBM); „Konto laboratorium” (`admin`:
    jeden token, pula sekund na okres, domyślny limit per osoba); „Limity osób” (`instruct`:
    odczyt matrycy + nadpisania); „Kurs” (`instruct`: ranking on/off); węzły i tryb izolacji,
    `max_qubits`, retencja (`admin`). **Bez** zakładki członków: karta „Dostęp” pokazuje tylko
    odczyt matrycy (kto ma co) i link „Zarządzaj w Addons” do zakładek Uprawnienia/Widoczność
    tej instancji — jeden edytor uprawnień w całym produkcie.

### 13.2 Nowe komponenty `tf-*`

- **`tf-quantum-circuit`** — siatka kubity × kolumny; renderowanie na `<canvas>` (wzór
  `tf-canvas`) z warstwą DOM dla focusu/a11y; drag-drop bramek z palety, przeciąganie po siatce,
  bramki wielokubitowe (kontrola/cel), parametry (`tf-input` w popoverze), zaznaczanie zakresu,
  undo/redo, eksport SVG; właściwość `circuit` (IR JSON) i zdarzenie `change`; tryb krokowy
  (`step` property podświetla kolumnę). Nie zna symulatora — dostaje `state` z zewnątrz.
- **`tf-bloch-sphere`** — jedna sfera per kubit, rzut 3D na `<canvas>` 2D (bez WebGL; obrót
  myszą), wektor + ślad ostatnich stanów; wejście: `(x, y, z)` z redukowanej macierzy gęstości
  (długość < 1 = stan mieszany, rysowany krócej).
- **`tf-mime-output`** — renderer mime bundle: `text/plain`, `text/markdown`, sanityzowany
  `text/html`, `image/png|jpeg`, `application/json` (drzewo), własne typy → `tf-bar-chart`
  (counts), `tf-bloch-sphere`/tabela amplitud (state), `tf-quantum-circuit` (circuit),
  `text/x-traceback` (błąd z kolorami). Wielkie wyjścia zwijane z przyciskiem „pokaż całość”.
- Wizualizacja wyników (§13.6): `tf-qsphere`, `tf-state-bars`, `tf-density-plot`,
  `tf-entanglement-graph`, `tf-state-timeline`, `tf-shot-histogram`.
- Rozszerzenia istniejących: tokenizer `openqasm` w `tf-code-editor`; `tf-select` z grupami
  opcji (jeśli brak); `tf-bloch-sphere` z animacją wektora.

### 13.3 Konwencje

Jeden `.tf-toolbar` na widok, `.ps-table-footer`-owy podsumowujący stopka pod listami,
dwuwierszowe komórki, KPI z `tf-stat-card`, liczby przez `fmtCompact`, liczby mnogie przez
`{count|f1|f2|f3}`, przestrzeń i18n `tentaquant.*` identyczna kluczami w pięciu locale
(`apps.tentaquant.{name,desc}` + namespace), test parytetu jak
`www/js/modules/tentanas/i18n-parity.test.js`. Ciemny i jasny motyw przez tokeny; `tf-bloch-sphere`
i `tf-quantum-circuit` czytają kolory z `--tf-*`.

### 13.4 Dostępność edytora obwodów

Każda bramka jest elementem z `role="gridcell"`, nawigacja strzałkami po siatce, wstawianie bramki
z klawiatury (paleta przez `tf-command-palette`), etykiety ARIA z nazwą bramki, kubitami
i parametrem. Rysowanie na canvasie jest prezentacją, nie źródłem prawdy.

### 13.5 Mobile

Widoki listowe i wyniki działają na telefonie; edytor obwodów i notatnik są „tylko podgląd” poniżej
768 px (edycja na desktopie). iOS bez usług Python (jak searxng) — tiery T0/T1 i przeglądanie.

### 13.6 Wyniki i wizualizacja: od laika do publikacji

Run ma dwa domy: **zakładkę „Wyniki” projektu** (galeria kafelków z miniaturą wykresu, przypinanie,
porównanie zaznaczonych; miniatura jest rysowana **po stronie klienta** z małego podsumowania
zapisanego przy zamknięciu runu (`runs.tile_json`: rodzaj runu + top-K counts / seria zbieżności /
wektory Blocha — jak mockup Q16: trzy kształty kafelka według rodzaju), bez SVG w CAS i bez
osobnego endpointu na obrazki (decyzja §18.27)) i **pełnoekranowy widok runu** (Q15) z pięcioma
widokami: Ewolucja, Stan, Histogram, Porównanie, Dane i eksport. Q08 zostaje listą całego laboratorium.

**Ewolucja stanu — animacja, nie slajdy.** Każda bramka to obrót, więc stan między „przed”
i „po” jest dobrze określony: `U^t = V · diag(λ^t) · V†` z rozkładu własnego macierzy bramki
(2×2 lub 4×4, liczony raz per bramka; dla bramek parametrycznych `R(θ·t)` wprost). Suwak czasu
przeciąga stan przez obwód w obie strony, wektory Blocha obracają się płynnie, słupki amplitud
morfują (kolor = faza, koło fazy w legendzie), pomiar to animowany kolaps (amplitudy
niezmierzonej gałęzi gasną, zmierzonej rosną do normy), a shoty napełniają histogram na żywo
(próbkowanie w przeglądarce z rozkładu końcowego, tempo od `speed`). Bramki na innych kubitach
w tym samym kroku nie ruszają się — użytkownik widzi, **co** zmieniła bramka.

Skąd bierze się stan do klatek:

| Run | Źródło klatek | Kto liczy klatki pośrednie |
|---|---|---|
| T0 (≤ 20 q) | pełny stan w przeglądarce (wasm, `step_fraction(t)`) | przeglądarka, dokładnie |
| T1 / T2 / T3 | **klatki kluczowe** po każdej bramce (`StateKeyframe`, niżej), streamowane w `RunEvent` | przeglądarka, dokładnie dla ilości zredukowanych |
| T3, > 20 q, tryb „na żywo” | GPU trzyma stan; przy przeciąganiu suwaka liczy `U^t` na pełnym stanie i streamuje pochodne po 30–60 fps | serwer |
| T4 (QPU) | sprzęt nie wystawia stanu — klatki z **symulacji z szumem** tego samego obwodu ISA (T1 `sim::noise`), pasek „symulacja, nie pomiar” | przeglądarka |

`StateKeyframe` (CBOR w `RunEvent{kind: state_keyframe}`, jedna na bramkę, ostatnia po
pomiarze): `step`, `gate` (id, kubity, macierz 2×2/4×4 `complex64`), `bloch: [n × (x,y,z)]`,
`pairs: [(i,j) → ρ_ij 4×4]` (informacja wzajemna i concurrence do mapy splątania), `top_k:
[(index, amplituda)]` z amplitudami partnerów po kubitach bramki (K = 256 dla Q-sfery i słupków),
`probs_top: [(bitstring, p)]`, `purity_per_qubit`. Rozmiar: ~2 KB dla 4 q, ~70 KB dla 32 q
(496 par × 16 × 8 B); przy n > 16 pary liczone tylko dla kubitów bramki i ich sąsiadów z mapy
sprzężeń, pełna mapa splątania na żądanie (`RunStateQuery{pairs: all}`) i na końcu obwodu.

Dlaczego przeglądarka może liczyć klatki pośrednie **dokładnie** bez pełnego stanu: bramka na
kubitach `A` komutuje ze śladem częściowym po pozostałych, więc `ρ_A(t) = U_t ρ_A U_t†` — wektory
Blocha i macierze par kubitów bramki między klatkami kluczowymi wynikają z 4×4 ρ_A, a kubity
spoza bramki nie zmieniają się. Amplitudy: bramka 1-kubitowa miesza tylko pary indeksów różniące
się bitem kubitu, 2-kubitowa czwórki — dlatego `top_k` niesie partnerów. Koszt klatki kluczowej po
stronie serwera: jeden przebieg po stanie na bramkę (Bloch dla wszystkich kubitów w jednym
przebiegu, pary kubitów bramki osobno) — na GPU pomijalny, na CPU T1 przy 28 q ~0,5 s na bramkę,
więc dla n > 24 na T1 klatki kluczowe są **opcją** („nagraj ewolucję”), nie domyślnym zachowaniem;
run bez klatek pokazuje tylko stan końcowy.

Transport: ten sam strumień `RunEvent` co wyjścia komórek (§11.2, `seq` + bufor odtworzenia),
binarnie po WebTransport/WS; 60 klatek kluczowych obwodu Grovera 4 q to ~100 KB — jedna ramka.
Tryb „na żywo” z T3 to pochodne (Bloch, pary bramki, `top_k`), nie stan, więc 20 KB × 60 fps
= 1,2 MB/s, w budżecie WebTransport; przeglądarka i tak interpoluje między odebranymi klatkami,
więc utrata tempa daje płynność, nie skoki. Tryb wymaga `quant.run.gpu` i żywej sesji runu
(GPU trzyma stan do `ttl_state`, domyślnie 10 min po zakończeniu, potem tylko klatki kluczowe
z CAS).

**Widoki stanu** (wszystkie z tych samych danych): sfera Blocha per kubit (kubit splątany
rysowany krótszym wektorem z chipem „splątany”), **Q-sfera** rejestru (stany bazowe na kuli
według wagi Hamminga, rozmiar = p, kolor = faza), słupki amplitud z kołem fazy, **macierz
gęstości** (city plot / heat Re i Im, do 6 q z pełnej ρ, powyżej — pary), **mapa splątania**
(graf na wierszach obwodu, grubość krawędzi = informacja wzajemna, kolor = concurrence).
Każdy widok ma przełącznik **„Wyjaśnij”**: zdanie po polsku generowane **deterministycznie
z cech stanu** (szablony: superpozycja / faza względna / splątanie z `purity`, `concurrence`
i `top_k`; podświetlenie tego, co zmieniła ostatnia bramka), bez LLM — asystent §12.4 to osobna
akcja „wyjaśnij więcej”, jawnie wołająca model.

**Histogram i porównania**: słupki z wąsami z liczby shotów (przedział Wilsona 95 %), nakładanie
ideał / symulacja z szumem / QPU, TVD i fidelity (Hellingera) w wierszu liczb, skala log,
dla runów wariacyjnych wykres zbieżności (energia per iteracja, aktualizowany z
`update_display_data`). Porównanie: do 8 runów naraz (chipy), jeden histogram z seriami,
tabela metryk (TVD, fidelity, czas, backend, data kalibracji) i wiersz różnic.

**Pakiet naukowy** (`RunExport`, jeden `.zip` z CAS): `counts.json` + `counts.csv`,
`statevector.npz` (gdy run ma stan, do limitu §18.9), `circuit.qasm` (wejściowy i ISA po
transpilacji), `calibration-snapshot.json` (właściwości backendu z chwili submitu — Core i tak
je pobiera do kosztorysu), wykresy `svg`/`png` w **motywie publikacyjnym** (jasne tło, podpisy
osi, czcionka bez ozdób, 300 DPI dla PNG, ten sam renderer co UI z innym zestawem tokenów),
`method.md` — notatka metodologiczna generowana z metadanych runu (backend, transpilacja
i poziom optymalizacji, shoty, mitigacja, seed, wersje Qiskit / silnika / Core, konto, czas
kolejki) + `citation.bib`. Notatka nie zawiera nic, czego nie ma w `runs.metrics_json` /
`qpu_jobs` — jest odtwarzalna, nie redagowana.

Komponenty (§13.2): `tf-bloch-sphere` dostaje animację wektora (slerp po obrocie, ślad), nowe
`tf-qsphere`, `tf-state-bars` (amplitudy + koło fazy), `tf-density-plot`, `tf-entanglement-graph`,
`tf-state-timeline` (pasek obwodu + suwak + transport), `tf-shot-histogram` (wąsy, serie, log);
wszystkie biorą `StateKeyframe`/`StateFrame` i nie znają symulatora. Crate (§6.1) wystawia
`step_fraction(t)`, `keyframe(step, opts)`, `reduced_dm(qubits)`, `mutual_information(i, j)`,
`concurrence(i, j)` — te same funkcje w wasm i natywnie, więc klatka z T1 i z T0 są bitowo równe
w teście złotym.

### 13.7 Powiązanie z Projektami

Projekt laboratorium może wskazywać projekt z Project Studio (`linked_project_id`): wtedy pliki
`.py`/`.qasm` są widoczne w Project Studio jako źródło kodu, a testy F3 mogą uruchamiać
`python_test` katy. Bez lustra ról w v1 — link jest tylko referencją; lustro (wzór `ml_link.rs`)
w F6, jeśli będzie potrzebne.

---

## 14. Platformy

| | Linux x86_64/aarch64 | macOS (Apple Silicon) | Windows | Android/iOS |
|---|---|---|---|---|
| T0/T1 | ✓ | ✓ | ✓ | ✓ (T1 na węźle zdalnym) |
| T0-py Pyodide (§4.6) | Chrome / Firefox / Safari desktop | ✓ | ✓ | domyślnie wyłączone (RAM, bateria); komórki Python przez fallback |
| T2 `quantum-python` | Docker + bundle | bundle | bundle | — |
| T3 `tentaflow-quantum` na GPU węzła | `cuda` (NVIDIA, sterownik przez `dlopen`) / `wgpu` Vulkan (AMD, Intel) | `wgpu` Metal | `cuda` / `wgpu` Vulkan lub DX12 | — (T0 w przeglądarce przez WebGPU) |
| T4 | ✓ (Core) | ✓ | ✓ | ✓ przez węzeł |
| Edycja slim | T0/T1/T4 działają; T2/T3 wymagają usług (ich manifesty są `resource_kind = infra`, więc slim je widzi) | | | |

T3 nie ma osobnej usługi ani obrazu Docker: to backend `cuda`/`wgpu` tego samego crate'a, w Core
(obwody) i w wheelu `tentaquant_sim` (komórki). Kod klasyczny na GPU przez PyTorch (ROCm/MPS/XPU)
i CuPy (ROCm). DGX Spark: `cuda` natywnie (aarch64, PTX `sm_80+`).

---

## 15. Observability i SLO

- Metryki per instancja: runy per tier, czas do pierwszego wyjścia komórki (TTFO), czas symulacji
  per kubity, zużycie QPU vs kosztorys (błąd estymacji), kolejka dostawcy, odrzucenia limitem,
  jądra żywe/idle, OOM/timeouty.
- Logi: ids, rozmiary, czasy; **nigdy** kod komórek, wyjścia, poświadczenia, nazwy backendów
  w URL-ach z tokenem.
- SLO: T0 < 50 ms dla ≤ 16 q po każdej zmianie; T1 ≤ 24 q < 1 s; start sesji jądra T2 < 8 s
  (ciepły obraz), T3 < 20 s; sondowanie jobu QPU ≤ 60 s od zakończenia u dostawcy; restart Core
  nie gubi ani jednego runu (reconcile) ani zgody.
- Wpięcie w Analitykę: runy i zużycie QPU jako `model_metrics_rollup`? **Nie** — inny model
  kosztu (sekundy/kredyty, nie tokeny). Własna zakładka w pulpicie laboratorium; do Analityki
  trafia tylko licznik zdarzeń AI z §12.4 (to już robi `AiGateway`).

---

## 16. Fazowanie

**Faza 0 — decyzje i spike'i (2 tygodnie).** ADR „aplikacja wieloinstancyjna” (§2.2, pkt 1–4
zaimplementowane i przetestowane na TentaNas jako regresja), ADR „własny symulator vs QDK”,
ADR „broker w Core, transpile w usłudze”, ADR „`container` domyślnie”, rozstrzygnięcia §18.
Spike A: `oq3_*` pod `wasm32-unknown-unknown` (tak/nie → §2.4). Spike B: statevector 24 q w wasm
w przeglądarce (czas bramki Hadamarda na 24 q < 100 ms?). Spike C (przeniesiony do F7 razem z IQM/QRMI). Spike D: kernel
WGSL bramki 1-kubitowej na 2^28 amplitud (2 GiB) na Vulkan (NVIDIA + AMD/Intel), Metal (Apple)
i WebGPU (Chrome) — czas i zgodność z CPU; ten sam kernel na 2^30 przez shardowanie. Spike E:
`maturin build` wheela pyo3 z crate'a w naszym `release.yml` per platforma + instalacja przez
`local_wheels` w python-bundle. Spike F: `qiskit._accelerate` pod Emscripten w Pyodide 314 (§4.6, budżet 2 tygodnie,
wynik = ADR „T0-py”). Spike G: kernel CUDA bramki 1-kubitowej (PTX przez `cudarc`, bez CUDA
Toolkit u użytkownika) na 2^28 amplitud — pasmo vs `wgpu` na tej samej karcie; jeśli różnica
< 20 %, backend `cuda` schodzi do F7 i T3 na NVIDIA idzie przez Vulkan. Platforma: kopiowanie rekurencyjne bundle'a (§2.5, luka 2) i hook
`init` zlecający deploy (luka 1), sprawdzone na `test-runner`.
*Kryterium:* dwie instancje TentaNas-testowego pakietu z `singleton = false` mają osobne bazy,
osobne kafelki, a bramka odrzuca `addon_id` innego pakietu.

**Faza 1 — instancja, rejestr, notatniki, T0/T1, Studio obwodów.** Manifest, hooki, migracje
`tentaquant.db`, `TentaQuantPayload` (`Lab*`, `Project*`, `File*`, `Notebook*`, `Circuit*`,
`Run*` dla T1), crate `tentaflow-quantum` (parse, IR, SV, stabilizer, grade, export OQ3/Qiskit,
wasm), `tf-quantum-circuit`, `tf-bloch-sphere` (z animacją), `tf-qsphere`, `tf-state-bars`,
`tf-state-timeline` (ewolucja T0: `step_fraction`, kolaps, shoty na żywo), `tf-mime-output`
(typy własne + text/png), zakładka „Wyniki” projektu z miniaturami,
notatnik z komórkami `circuit`/`markdown`, Studio obwodów, ekran Runy (T1), przykłady 1, 2 (cpu
jako plik do podglądu, uruchamialne tylko `circuit.qasm`), 10 pierwszych zadań kursu, kroki
aplikacji w kreatorze instancji (§2.2 pkt 4) z krokiem „test Bell”.
*Kryterium:* użytkownik tworzy laboratorium, buduje Bella w Studio, widzi Blocha i histogram
natychmiast, zapisuje, uruchamia na T1 na innym węźle, wyniki są bitowo zgodne z T0 i z Aer
(test złoty); kata „przygotuj |+⟩” ocenia się w < 100 ms.

**Faza 2 — `quantum-python`, sesje jądra, komórki Python, SDK.** Manifest usługi + bundle + obraz
Docker (Qiskit 2.5, runtime 0.49, Aer 0.17, PennyLane 0.45, `qdk` 1.31, `cirq`, `qbraid-qir`),
reguła `trusted_native` (jedna osoba w matrycy albo potwierdzenie admina, §8.2),
`server.py` z `jupyter_client`, `SandboxLimits::quantum_kernel`, kanał zwrotny (`reverse_requests`,
`lookup_owned_kernel_session`), `Kernel*` + strumień lokalny, SDK `tentaquant` (bez QPU) z
`BackendV2` nad `tentaquant_sim` (wheel pyo3, CPU + wgpu — GPU na każdym producencie od tej
fazy), **auto-deploy usługi z hooka instalacji instancji** (zadanie w tle, stan na kafelku),
tokenizer `openqasm`, `tf-mime-output` z `text/html` sanityzowanym, przykłady 1–5 `cpu.py` +
`gpu.py` przez wgpu, warianty `classical_*` dla 4 i 5, eksport `.ipynb`, **fallback „jądro liczy,
przeglądarka wykonuje”** (`QuantumBrowserRun`, §4.6) i — gdy ADR ze spike'u F mówi „tak” — **T0-py**:
worker Pyodide, kółka `qiskit`/`rustworkx`, skan importów, stan `needs_kernel`.
*Kryterium:* instalacja instancji w Addons na maszynie z AMD/Intel/Apple GPU kończy się bez
żadnego kroku ręcznego działającym `tentaquant.backend("gpu")` (GHZ 26 q na wgpu zgodne z CPU do
`1e-5`); instancja z dwiema osobami w matrycy na maszynie bez Dockera nie startuje T2 bez
jawnego potwierdzenia admina; komórka z `matplotlib` pokazuje wykres w < 2 s od wykonania; `input()` daje
`EOFError`; jądro, które próbuje `urllib.request.urlopen("https://quantum.cloud.ibm.com")`, dostaje błąd
sieci; Core zabity w trakcie komórki po restarcie zamyka run jako `failed` z powodem `orphaned`.

**Faza 3 — T3 na własnym silniku, szum, relay strumieni w mesh, węzły.** Backend `cuda`
(kernele → PTX w repo, `cudarc` z `dynamic-loading`) i natywny `wgpu` (Vulkan/Metal/DX12) w Core,
te same backendy w wheelu `tentaquant_sim`, `sim::noise` (kanały Krausa, model z kalibracji IBM),
reguła `device="auto"` (§5.3), placement na węzeł z GPU, uogólnienie `mesh_stream` do `AppStream*`
z przepięciem Code Studio, `Target*` z GPU z heartbeatu, przykłady 2, 4, 6, 7, 9 `gpu.py`,
benchmark tierów (przykład 9) z wykresem CUDA vs Vulkan vs Metal, `StateKeyframe` z T1/T2/T3
(`RunEvent`), `tf-density-plot`, `tf-entanglement-graph`, tryb „na żywo” z GPU (`state_frame`).
*Kryterium:* GHZ 28 q z dashboardu na węźle A wykonuje się na GPU węzła B (NVIDIA przez `cuda`,
AMD przez Vulkan, Mac przez Metal — jeden zestaw złoty, zgodność do `1e-5`), strumień przeżywa
rozłączenie dashboardu i wznawia od `after_seq`; symulacja Bella z modelem szumu `ibm_torino`
na T1 daje TVD < 0,02 wobec Aer z tym samym modelem; testy e2e Code Studio zielone po przepięciu
relay.

**Faza 4 — broker QPU (IBM), konta osobiste i konto laboratorium, pula, zgody, kosztorys.**
`providers` (`user` + `instance`), `qpu_budget`, `qpu_user_limits`, `quota_ledger`, `approvals`,
`QpuProvider` + `ibm.rs`, transpile w usłudze, sondowanie jobów w tle per instancja, ekran
Urządzenia z kalibracją, dialog kosztorysu z wyborem konta, karta „Do zatwierdzenia” na Pulpicie
+ powiadomienia, porównanie runów (do 8, nałożone serie, tabela metryk), `tf-shot-histogram`
z wąsami, ewolucja runu QPU z symulacji z szumem, pakiet naukowy `RunExport` (motyw
publikacyjny, `method.md`, `citation.bib`), przykłady 1, 3, 5 `qpu.py`, SDK
`tq.run(target="qpu:...")`.
*Kryterium:* użytkownik z własnym tokenem wysyła Bella na `ibm_kingston` bez zgody i bez ruchu
w puli; użytkownik bez tokenu dostaje kosztorys z konta laboratorium, run czeka na zgodę opiekuna,
po odmowie nic nie poszło do IBM (brak `provider_job_id`); limit osoby 60 s/28 dni blokuje drugi
submit przed zgodą; timeout w `submit` kończy się `unknown` + sondą, nie duplikatem; po
zakończeniu `usage.seconds` koryguje ledger.

**Faza 5 — hybryda w flow, QIR.** Bloki `quantum_run` / `quantum_program` /
`quantum_target_switch` z `InteractionGate` dla QPU, emisja QIR w usłudze, przykłady 6, 7 w pełnej hybrydzie (cpu→gpu→qpu w jednym pliku i jako flow),
`tq.hybrid.minimize`.
*Kryterium:* flow „LLM proponuje obwód → walidacja T1 → symulacja T3 → zgoda → QPU → raport”
działa end-to-end, a blok QPU bez zgody wstrzymuje run zamiast go pominąć.

**Faza 6 — kurs i przykłady do kompletu.** 24 zadania kursu (kata 20 na T3, 21–24 na
symulatorze z szumem + „sprawdź naprawdę”), punkty, seria, ranking z przełącznikiem, tablica
postępu, katy `python_test`, tryb „Wyjaśnij” (szablony z cech stanu) w każdym widoku, przykłady
8, 10, asystent (§12.4), przykłady laboratorium (opiekun), link do Project Studio.
*Kryterium:* nowy użytkownik przechodzi ścieżkę „bramki → Bell na QPU” bez pomocy opiekuna,
z 0 zgłoszeń wsparcia w teście z 10 osobami (kryterium produktowe, nie techniczne).

**Faza 7 — platformy i dopracowanie.** Bundle macOS/Windows T2, dostawcy v2 (§7.4: IonQ, IQM przez
QRMI — spike C tutaj), scheduler placementu (kolejka, szacowanie czasu), eksport/import laboratorium (wzór `archive.rs`: sekrety wyzerowane, limity
zresetowane), retencja i GC.

---

## 17. Testy

- **Crate**: złote wobec Aer (§6.2), wasm ↔ natywny, round-trip OQ3, własność „stabilizer ==
  statevector” na losowych Cliffordach, benchmark.
- **Platforma multi-instance**: dwie instancje, izolacja baz i katalogów, bramka z obcym
  `addon_id` → `PolicyDenied`/`NotFound`, uninstall jednej nie rusza drugiej (`close()` +
  wymazanie tylko jej katalogu); **matryca per instancja**: grupa z `quant.run` w instancji A
  i bez wpisu w B liczy w A, a w B dostaje `AppUnavailable`; `deny` per użytkownik wygrywa
  z `allow` grupy; `LabPeople` zwraca dokładnie ekspansję matrycy (test przeciw
  `PermissionChecker`).
- **Protokół**: test złoty `tentaquant_wire_golden` (tagi po nazwie), parytet i18n pięciu locale.
- **Usługa**: `tests/test_contract.py` (jak test-runner): sesje, execute, interrupt, SSE, limity
  czasu/RAM, brak sieci, `session_token` obcy → 403.
- **Broker**: mock serwera IBM (wiremock) — happy path, 429 z `Retry-After`, timeout w
  submit → `unknown` + sonda, ledger pod obciążeniem (100 równoległych submitów, ani pula, ani
  limit osoby nigdy przekroczone), zgoda wygasła, run z tokenu osobistego nie dotyka ledgera.
- **E2E** (`tests/e2e/tentaquant.spec.js`): laboratorium → Studio → Bell → T0 stan → zapis → T1 run
  → porównanie; notatnik Python z wykresem; fork przykładu; zadanie kursu; `viewer` widzi przycisk
  „uruchom” tylko dla T0 i nie tworzy runu; fixture SQL jak
  `analytics-seed.sql`.
- **Integracyjne `#[ignore]`**: prawdziwy IBM Open (env `TENTAFLOW_IBM_API_KEY`, `..._CRN`) na
  1 job Bella; uruchamiane ręcznie, nie w CI.
- **Bezpieczeństwo**: `text/html` z `<script>`/`onerror` nie wykonuje się; SVG odrzucone; sekret
  dostawcy nie występuje w żadnym logu ani odpowiedzi (grep po fixture'owym kluczu w całym
  wyjściu testów, jak w Code Studio F1).

---

## 18. Decyzje przed Fazą 1

Rozstrzygnięte **2026-09-03** z właścicielem produktu (oznaczone ✔); reszta czeka na spike'i.

1. ✔ **Katalog uprawnień**: sześć id — `quant.read`, `quant.run`, `quant.run.gpu`,
   `quant.run.qpu`, `quant.instruct`, `quant.admin` (§10.2). Opiekun (`instruct`) nie musi być
   adminem org. Dawne `providers.manage` zwinięte w `admin`, bo po decyzji 5 jedynym sekretem
   instancji jest konto laboratorium.
2. **Symulator**: własny crate (rekomendacja, §1.5/§6) czy `qdk_simulators` jako zależność git.
   Własny = ~4–6 tygodni na F1 z testami złotymi; QDK = mniej kodu, ale git-only, sprzężony
   z workspace Q# i bez gwarancji wasm w naszym pipeline.
3. ✔ **QRMI**: nie w v1. Spike C i IQM przez `iqm-server` w F7 (§7.4).
4. ✔ **Domyślny tryb izolacji**: `container` gdy jest runtime; `trusted_native` bez pytania tylko
   dla jednej osoby w matrycy, inaczej jawne potwierdzenie `quant.admin` z ostrzeżeniem; bez
   runtime i bez potwierdzenia instancja nie ma T2 (§8.2).
5. ✔ **Konta QPU**: domyślnie osobiste (każdy swój token IBM, run z jego konta bez zgody i puli);
   opcjonalne jedno konto laboratorium z pulą sekund na okres i limitem per osoba, run z niego
   zawsze za zgodą opiekuna (§7.3).
6. ✔ **Dostawcy v1**: tylko IBM. Trait `QpuProvider` zostaje z jednym implementatorem; IonQ/IQM
   w F7.
7. **Q#/`qsharp-lang`**: nie (rekomendacja) — jeden język mniej do utrzymania; katy w OQ3/Python.
8. **Nazwa pakietu i trasy**: `tentaquant` (id, route, namespace), uprawnienia `quant.*`.
9. **Limit wektora stanu zapisywanego** do CAS (64 MiB) i domyślna retencja (180 dni).
10. **Jak Python dociera do naszego symulatora**: moduł pyo3 w jądrze (rekomendacja: pętle
    wariacyjne robią tysiące małych obwodów, koszt wywołania w procesie to mikrosekundy; pierwszy
    pyo3/maturin w repo) czy wołanie symulatora w Core przez kanał zwrotny (bez nowego
    toolchainu, ale milisekundy na obwód i VRAM trzymany przez proces Core).
11. **Precyzja na GPU**: `complex64` na `wgpu`, `complex64`/`complex128` na `cuda` (UI pokazuje
    precyzję targetu); `SHADER_F64` na Vulkanie odrzucone (16–64× wolniej, Apple bez tego).
12. **Pakiet offline**: v1 wymaga internetu przy pierwszym deployu usługi (jak każdy
    python-bundle); gotowe archiwa venv jako assety release (wiele GB per platforma × backend)
    dopiero jako opcja później — potwierdzić, że to akceptowalne dla sieci zamkniętych.
13. **Dostawca GPU dla kodu klasycznego**: PyTorch jako jedyna wspólna warstwa (CUDA/ROCm/
    MPS/XPU) + CuPy/Numba tylko w przykładach oznaczonych „NVIDIA” — czy to wystarcza.
14. **Klasyczny kod na GPU przez własny moduł zamiast PyTorch?** Ten sam wheel może
    wystawić `tentaquant.gpu.kernel(wgsl).run(buffers)` — użytkownik pisze shader WGSL, działa
    na każdej karcie i 1:1 w przeglądarce (WebGPU), bez 2 GB PyTorcha. Cena: WGSL zamiast
    numpy-podobnego API, więc trudniejszy dla początkujących. Rekomendacja: PyTorch jako warstwa
    domyślna (decyzja 13), WGSL jako opcjonalny „tryb ekspert” w przykładach porównawczych,
    bo tylko on daje identyczny kod serwer↔przeglądarka.
15. ✔ **Własność projektu (wzór ML Studio)**: prywatny domyślnie, właściciel udostępnia
    (`editor`/`viewer`) lub całemu laboratorium (odczyt). To nie jest członkostwo (matryca
    w Addons). Opiekun **nie** widzi treści cudzych prywatnych projektów — tylko metadane runów
    i postęp kursu (spójne z §10.3). `viewer` uruchamia wyłącznie T0 w przeglądarce, bez zapisu.
16. **Cross-origin isolation dashboardu dla Pyodide** (§4.6): COOP/COEP globalnie (przerywanie
    komórki bez utraty jądra, ale audyt każdego osadzanego zasobu i iframe'u) czy bez nich
    (przerwanie = restart workera). Rekomendacja: zacząć bez, zmierzyć w spike'u F, ile
    trwa restart z ciepłym cache — jeśli < 3 s, COOP/COEP nie są warte ryzyka regresji.
17. ✔ **T3 = własny silnik**: `tentaflow-quantum` z backendami `cuda` (NVIDIA), `wgpu`-Vulkan
    (AMD/Intel), `wgpu`-Metal (Apple) — w Core i w wheelu pyo3. Bez usługi `quantum-gpu`,
    bez cuQuantum/CUDA-Q jako targetów (§4.1, §6.3, §14). Spike G decyduje, czy `cuda` wchodzi
    w F3, czy NVIDIA startuje na Vulkanie.
18. ✔ **Symulacja z szumem**: T1 przez własne kanały Krausa w Core (`sim::noise`, model
    z kalibracji IBM) **i** T2 przez Aer; T0/T3 idealnie w v1 (T3 dostanie trajektorie, gdy
    kernele będą — ten sam kod). Test złoty T1 vs Aer.
19. ✔ **`device="auto"`**: prosta, deterministyczna reguła (§5.3), nigdy QPU; scheduler z kolejką
    w F7.
20. ✔ **Ranking kursu**: jak w mockupie (top 5 z nazwiskami + własna pozycja), włączony domyślnie,
    opiekun wyłącza w Ustawieniach (`ranking_enabled`).
21. ✔ **Katy T3/T4**: kata 20 wymaga `quant.run.gpu`; katy 21–24 zaliczane na symulatorze
    z szumem, prawdziwy QPU opcjonalnie z własnego konta („sprawdź naprawdę”).
22. ✔ **Kreator instancji**: generyczny kreator Addons rozszerzony o kroki deklarowane
    w manifeście aplikacji (§2.2 pkt 4); Q14 = dane + matryca + deploy `quantum-python` + nody GPU
    + test Bell.
23. ✔ **Zgody**: karta „Do zatwierdzenia” na Pulpicie (tylko `quant.instruct`) + powiadomienie
    Core; szczegół = Q13 w trybie odczytu. Bez osobnej zakładki.
24. ✔ **Bez LMS**: żadnych terminów, przydziałów zadań, ocen ani „klas”. Kurs to 24 zadania
    w stałej kolejności dla każdego użytkownika; słownictwo: *użytkownik*, *opiekun*, *Kurs*
    (nie student / prowadzący / Nauka) — w UI, i18n i tym dokumencie.
25. ✔ **Wizualizacja wyników** (§13.6): ciągła animacja ewolucji bramka po bramce (T0 z pełnego
    stanu, T1–T3 z klatek kluczowych, T3 dodatkowo „na żywo” z GPU, T4 z symulacji z szumem),
    widoki Bloch + Q-sfera + amplitudy z fazą + macierz gęstości + mapa splątania, histogram
    z wąsami i porównania do 8 runów, tryb „Wyjaśnij” z szablonów, pełny pakiet naukowy
    (dane surowe, wykresy publikacyjne, `method.md`, BibTeX); zakładka „Wyniki” w projekcie
    + pełnoekranowy widok runu.
27. ✔ **Miniatury i mime obwodu (2026-09-05)**: kafelek wyniku liczony po stronie klienta
    z `runs.tile_json` (mockup Q16 wygrywa z pierwotnym opisem §13.6 — SVG w CAS i endpoint
    obrazków do usunięcia przy Q16); własny typ mime obwodu to
    `application/x-tentaquant-circuit+json` (IR z `parse()`), nie `+qasm3`.
26. ✔ **Instancja bez właściciela**: kafelek i nagłówek laboratorium pokazują liczbę osób
    z dostępem i moją rolę z matrycy; żadnego pola „właściciel” (§3.1).

---

## 19. Kwestie otwarte (mockupy)

1. Układ notatnika: prawy panel stanu stały czy wysuwany; gdzie żyje wybór targetu (pasek
   notatnika vs per komórka).
2. Studio obwodów: paleta bramek pionowa (Quirk) czy pozioma (Composer); prezentacja bramek
   parametrycznych i własnych; jak pokazać „ten obwód jest Cliffordem — 500 kubitów OK”.
3. ✔ Sfera Blocha: rząd równych sfer, kubit splątany = krótszy wektor + chip „splątany”; obok
   Q-sfera rejestru i mapa splątania (§13.6).
4. Dialog kosztorysu QPU: jedna karta z porównaniem backendów czy krok kreatora; jak pokazać
   „poczekasz ~40 min w kolejce”.
5. ✔ Porównanie: jeden histogram z nałożonymi seriami + tabela metryk + wiersz różnic (§13.6).
6. Urządzenia: mapa sprzężeń jako graf (`tf-relation-graph`) czy siatka heavy-hex — ta druga
   jest specyficzna dla IBM.
7. Kurs: postęp jako ścieżka (mapa) czy lista; jak pokazać wynik zadania „prawie dobrze” (fidelity
   0.98).
11. ✔ Wyniki: zakładka „Wyniki” w projekcie (galeria, przypinanie, porównanie) + pełnoekranowy
    widok runu Q15 z animowaną ewolucją, także dla runów serwerowych przez klatki kluczowe
    (§13.6). Mockupy: Q15, Q16.
8. Lista laboratoriów vs bezpośrednie wejście, gdy użytkownik ma jedno.
9. Prezentacja trybu `trusted_native` w laboratorium wieloosobowym (stałe ostrzeżenie w nagłówku,
   jak Code Studio §19).
10. Widok opiekuna: tablica postępu i runów osób — osobny ekran czy filtr w Runach/Kursie.
