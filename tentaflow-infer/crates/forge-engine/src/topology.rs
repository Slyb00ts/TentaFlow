// ===== File: topology.rs — karty w jednym nodzie i w wielu nodach =====
//
// `multi_gpu.rs` odpowiada na pytanie ILE pracy dostaje każda karta. Ten moduł
// odpowiada na pytanie wcześniejsze i ważniejsze: KTÓRE karty wolno w ogóle
// wpiąć w ten sam podział, a które trzeba rozdzielić na etapy.
//
// DLACZEGO TO JEST OSOBNA DECYZJA. Tensor parallel wymienia aktywacje DWA RAZY
// na warstwę. Przy 65 warstwach to 130 wymian na token. Zmierzone na tej
// maszynie: 6,45 us przez P2P między kartami w jednym nodzie, czyli 0,84 ms na
// token — 5% czasu dekodowania, akceptowalne. Przez zwykłą sieć 10 GbE ta sama
// wymiana to ~120 us, czyli 15,6 ms na token — WIĘCEJ niż całe liczenie.
//
// Wniosek nie jest kwestią gustu, tylko arytmetyki: TP działa tam, gdzie łącze
// jest szybkie (wewnątrz noda, RDMA), a między nodami po wolnym łączu jedyną
// techniką, która się broni, jest pipeline — bo płaci JEDNĄ wymianę na granicę
// etapu zamiast dwóch na warstwę.
//
// Ten moduł NIE zawiera protokołu sieciowego. Zawiera model kosztu i decyzję,
// którą ten protokół będzie musiał obsłużyć.

use crate::multi_gpu::{DeviceCapability, MIN_USEFUL_ROWS, SplitPlan, WorkKind, plan_split};
use forge_types::{ForgeError, Result};

/// Node w sensie maszyny. Karty w jednym nodzie łączy szyna, karty w różnych —
/// sieć.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// Adres jednej karty w całym klastrze. Karta jest zawsze w jakimś nodzie —
/// maszyna jednonodowa to po prostu klaster z jednym `NodeId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkerId {
    pub node: NodeId,
    /// Indeks karty w obrębie noda.
    pub device: usize,
}

/// Rodzaj łącza. Służy WYŁĄCZNIE do diagnostyki i do sensownej wartości
/// startowej — decyzje zapadają na zmierzonych `bytes_per_s` i `latency_s`,
/// nigdy na tym enumie. Inaczej powtórzylibyśmy błąd wnioskowania z nazwy
/// sprzętu zamiast z pomiaru.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkClass {
    /// Ta sama karta — wymiany nie ma.
    SameDevice,
    /// Dwie karty w jednym nodzie (P2P przez szynę albo przez host).
    IntraNode,
    /// Między nodami, transport z pominięciem jądra.
    Rdma,
    /// Między nodami, zwykły stos sieciowy.
    Ethernet,
}

/// Zmierzony koszt przesłania danych między dwoma kartami.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub class: LinkClass,
    pub bytes_per_s: f64,
    /// Koszt stały jednej wymiany, niezależny od rozmiaru. Przy aktywacjach
    /// rzędu kilku KiB to on zwykle dominuje, nie pasmo.
    pub latency_s: f64,
}

impl Link {
    pub fn same_device() -> Self {
        Self {
            class: LinkClass::SameDevice,
            bytes_per_s: f64::INFINITY,
            latency_s: 0.0,
        }
    }

    /// Czas jednej wymiany `bytes` bajtów.
    pub fn exchange_seconds(&self, bytes: usize) -> f64 {
        if self.class == LinkClass::SameDevice {
            return 0.0;
        }
        if !(self.bytes_per_s > 0.0) {
            return f64::INFINITY;
        }
        self.latency_s + bytes as f64 / self.bytes_per_s
    }
}

/// Jedna karta w klastrze wraz z jej zmierzonymi możliwościami.
#[derive(Clone, Copy, Debug)]
pub struct Worker {
    pub id: WorkerId,
    pub capability: DeviceCapability,
}

/// Karty klastra i zmierzone łącza między nimi.
pub struct Topology {
    workers: Vec<Worker>,
    /// Macierz `n x n`, wiersz-major. Symetryczna nie jest wymuszana — łącze
    /// bywa niesymetryczne i lepiej to zachować niż uśrednić.
    links: Vec<Link>,
}

impl Topology {
    pub fn new(workers: Vec<Worker>, links: Vec<Link>) -> Result<Self> {
        let n = workers.len();
        if n == 0 {
            return Err(ForgeError::Scheduler("topologia bez kart".into()));
        }
        if links.len() != n * n {
            return Err(ForgeError::Scheduler(format!(
                "macierz łączy ma {} pozycji, oczekiwano {}",
                links.len(),
                n * n
            )));
        }
        Ok(Self { workers, links })
    }

    /// Topologia jednonodowa: wszystkie karty w `NodeId(0)`, jedno wspólne
    /// łącze między każdą parą. To jest dzisiejszy przypadek dwóch kart w
    /// jednej maszynie — ta sama struktura, nie osobna ścieżka.
    pub fn single_node(caps: &[DeviceCapability], intra: Link) -> Result<Self> {
        let workers = caps
            .iter()
            .enumerate()
            .map(|(device, &capability)| Worker {
                id: WorkerId {
                    node: NodeId(0),
                    device,
                },
                capability,
            })
            .collect();
        let n = caps.len();
        let mut links = Vec::with_capacity(n * n);
        for a in 0..n {
            for b in 0..n {
                links.push(if a == b { Link::same_device() } else { intra });
            }
        }
        Self::new(workers, links)
    }

    pub fn workers(&self) -> &[Worker] {
        &self.workers
    }

    pub fn len(&self) -> usize {
        self.workers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    pub fn link(&self, from: usize, to: usize) -> Link {
        self.links[from * self.workers.len() + to]
    }

    pub fn capabilities(&self) -> Vec<DeviceCapability> {
        self.workers.iter().map(|w| w.capability).collect()
    }
}

/// Co musi być znane, żeby ocenić opłacalność tensor parallel.
#[derive(Clone, Copy, Debug)]
pub struct LayerProfile {
    /// Bajty aktywacji wymieniane w JEDNYM punkcie synchronizacji.
    pub exchange_bytes: usize,
    /// Ile trwa warstwa na jednej karcie, bez podziału.
    pub layer_seconds: f64,
    /// Liczba warstw modelu — potrzebna do kosztu pipeline'u.
    pub layers: usize,
}

/// Ile punktów wymiany ma warstwa w tensor parallel: po bloku uwagi i po FFN.
const TP_EXCHANGES_PER_LAYER: usize = 2;

/// Ułamek czasu warstwy, powyżej którego narzut wymiany przestaje się opłacać.
/// 25% oznacza: zgadzamy się oddać najwyżej ćwierć zysku na komunikację.
const TP_OVERHEAD_BUDGET: f64 = 0.25;

/// Czy warto dołożyć tę parę kart do wspólnego tensor parallel.
///
/// Idealny podział między dwie równe karty skraca warstwę o połowę. Wymiana
/// musi się zmieścić w ustalonym ułamku TEGO ZYSKU, nie całego czasu warstwy —
/// inaczej dokładalibyśmy kartę, która oddaje w komunikacji więcej, niż wnosi.
pub fn tensor_parallel_viable(link: Link, profile: LayerProfile) -> bool {
    if link.class == LinkClass::SameDevice {
        return true;
    }
    let overhead = TP_EXCHANGES_PER_LAYER as f64 * link.exchange_seconds(profile.exchange_bytes);
    let gain = profile.layer_seconds / 2.0;
    overhead <= gain * TP_OVERHEAD_BUDGET
}

/// Grupa kart licząca te same warstwy razem (tensor parallel), będąca zarazem
/// jednym etapem pipeline'u.
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    /// Indeksy kart w `Topology::workers`.
    pub workers: Vec<usize>,
    /// Zakres warstw modelu liczony przez ten etap.
    pub first_layer: usize,
    pub layers: usize,
}

/// Pełny plan wykonania: etapy pipeline'u, w każdym grupa tensor parallel.
///
/// Jeden etap z wszystkimi kartami = czysty tensor parallel. Tyle etapów, ile
/// kart, każdy jednoelementowy = czysty pipeline. Wszystko pomiędzy wychodzi z
/// pomiaru łączy, bez osobnych trybów w konfiguracji.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionPlan {
    pub stages: Vec<Stage>,
}

impl ExecutionPlan {
    /// Czy plan sprowadza się do jednej grupy liczącej wszystko razem.
    pub fn is_pure_tensor_parallel(&self) -> bool {
        self.stages.len() == 1
    }

    /// Czy każdy etap to pojedyncza karta.
    pub fn is_pure_pipeline(&self) -> bool {
        self.stages.iter().all(|s| s.workers.len() == 1)
    }
}

/// Dzieli klaster na etapy pipeline'u i grupy tensor parallel WYŁĄCZNIE na
/// podstawie zmierzonych łączy i profilu warstwy.
///
/// Karty trafiają do jednej grupy TP, jeśli łączy je łącze wystarczająco
/// szybkie w OBIE strony. Grupowanie jest przechodnie (spójne składowe grafu
/// szybkich łączy) — przy topologiach mieszanych, gdzie A-B i B-C są szybkie, a
/// A-C nie, daje to grupę zbyt optymistyczną. Nie zgaduję tu nic w drugą stronę:
/// taka topologia wymaga pomiaru wszystkich par i świadomej decyzji, więc
/// zwracam błąd zamiast cicho zbudować plan, który się nie spina.
pub fn plan_execution(
    topology: &Topology,
    profile: LayerProfile,
    kind: WorkKind,
) -> Result<ExecutionPlan> {
    let n = topology.len();
    if profile.layers == 0 {
        return Err(ForgeError::Scheduler("model bez warstw".into()));
    }

    // Spójne składowe grafu łączy nadających się na TP.
    let mut group_of = vec![usize::MAX; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if group_of[start] != usize::MAX {
            continue;
        }
        let index = groups.len();
        let mut members = vec![start];
        group_of[start] = index;
        let mut cursor = 0;
        while cursor < members.len() {
            let current = members[cursor];
            cursor += 1;
            for other in 0..n {
                if group_of[other] != usize::MAX {
                    continue;
                }
                let forward = tensor_parallel_viable(topology.link(current, other), profile);
                let backward = tensor_parallel_viable(topology.link(other, current), profile);
                if forward && backward {
                    group_of[other] = index;
                    members.push(other);
                }
            }
        }
        groups.push(members);
    }

    // Kontrola spójności: w obrębie grupy KAŻDA para musi być zdatna na TP.
    for members in &groups {
        for (slot, &a) in members.iter().enumerate() {
            for &b in &members[slot + 1..] {
                if !tensor_parallel_viable(topology.link(a, b), profile)
                    || !tensor_parallel_viable(topology.link(b, a), profile)
                {
                    return Err(ForgeError::Scheduler(format!(
                        "łącza niespójne: karty {a} i {b} trafiłyby do jednej grupy \
                         tensor parallel przez pośrednika, a bezpośrednie łącze \
                         między nimi jest za wolne"
                    )));
                }
            }
        }
    }

    if groups.len() == 1 {
        return Ok(ExecutionPlan {
            stages: vec![Stage {
                workers: groups.pop().expect("jedna grupa"),
                first_layer: 0,
                layers: profile.layers,
            }],
        });
    }

    // Warstwy między etapami dzielimy tą samą proporcją mocy co wiersze wewnątrz
    // etapu — grupa dwa razy mocniejsza bierze dwa razy więcej warstw, więc
    // etapy trwają tyle samo i pipeline nie ma wąskiego gardła.
    let stage_caps: Vec<DeviceCapability> = groups
        .iter()
        .map(|members| aggregate(topology, members, kind))
        .collect();
    // Próg opłacalności liczony w WARSTWACH, nie w wierszach: etap z jedną
    // warstwą ma sens, bo płaci jedną wymianę na granicę, a nie dwie na warstwę.
    let layer_split = plan_split(&stage_caps, profile.layers, kind, 0, 1)?;

    let mut stages = Vec::with_capacity(groups.len());
    let mut first_layer = 0;
    for (index, members) in groups.into_iter().enumerate() {
        let layers = layer_split.rows[index];
        if layers == 0 {
            continue;
        }
        stages.push(Stage {
            workers: members,
            first_layer,
            layers,
        });
        first_layer += layers;
    }
    if stages.is_empty() {
        return Err(ForgeError::Scheduler(
            "podział warstw nie przydzielił żadnemu etapowi pracy".into(),
        ));
    }
    Ok(ExecutionPlan { stages })
}

/// Moc grupy jako całości: przepustowości się sumują, pamięć też. To jest
/// przybliżenie od góry — zakłada, że TP wewnątrz grupy skaluje się liniowo.
/// Pętla korekty z `update_capability` i tak sprowadzi to do rzeczywistości.
fn aggregate(topology: &Topology, members: &[usize], _kind: WorkKind) -> DeviceCapability {
    let mut stream = 0.0;
    let mut matmul = 0.0;
    let mut free = 0usize;
    for &index in members {
        let cap = topology.workers[index].capability;
        stream += cap.stream_bytes_per_s;
        matmul += cap.matmul_ops_per_s;
        free = free.saturating_add(cap.free_bytes);
    }
    DeviceCapability {
        stream_bytes_per_s: stream,
        matmul_ops_per_s: matmul,
        free_bytes: free,
    }
}

/// Podział wierszy WEWNĄTRZ jednego etapu — to już zwykły `plan_split` na
/// kartach tego etapu.
pub fn plan_stage_rows(
    topology: &Topology,
    stage: &Stage,
    rows: usize,
    kind: WorkKind,
    bytes_per_row: usize,
) -> Result<SplitPlan> {
    let caps: Vec<DeviceCapability> = stage
        .workers
        .iter()
        .map(|&index| topology.workers[index].capability)
        .collect();
    plan_split(&caps, rows, kind, bytes_per_row, MIN_USEFUL_ROWS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(stream: f64, matmul: f64, free_gib: usize) -> DeviceCapability {
        DeviceCapability {
            stream_bytes_per_s: stream,
            matmul_ops_per_s: matmul,
            free_bytes: free_gib << 30,
        }
    }

    /// Zmierzone P2P między kartami w tej maszynie: 14,2 GB/s, 6,45 us na
    /// wymianę 10 KiB (czyli latencja dominuje).
    fn p2p() -> Link {
        Link {
            class: LinkClass::IntraNode,
            bytes_per_s: 14.2e9,
            latency_s: 6.0e-6,
        }
    }

    fn rdma() -> Link {
        Link {
            class: LinkClass::Rdma,
            bytes_per_s: 12.5e9,
            latency_s: 15.0e-6,
        }
    }

    fn ethernet() -> Link {
        Link {
            class: LinkClass::Ethernet,
            bytes_per_s: 1.1e9,
            latency_s: 120.0e-6,
        }
    }

    /// Warstwa 27B w dekodowaniu: ~0,23 ms na kartę, 10 KiB aktywacji.
    fn profile() -> LayerProfile {
        LayerProfile {
            exchange_bytes: 10 << 10,
            layer_seconds: 0.23e-3,
            layers: 65,
        }
    }

    #[test]
    fn p2p_uniesie_tensor_parallel() {
        assert!(tensor_parallel_viable(p2p(), profile()));
    }

    #[test]
    fn ethernet_nie_uniesie_tensor_parallel() {
        // 2 * 129 us wobec zysku 0,115 ms — narzut przekracza cały zysk.
        assert!(!tensor_parallel_viable(ethernet(), profile()));
    }

    #[test]
    fn rdma_zalezy_od_czasu_warstwy() {
        // Ta sama karta i to samo łącze: przy krótkiej warstwie RDMA się nie
        // spina, przy długiej (większy model) już tak. Dowód, że decyzja jest
        // ilościowa, a nie oparta na nazwie transportu.
        let mut short = profile();
        short.layer_seconds = 0.23e-3;
        assert!(!tensor_parallel_viable(rdma(), short));
        let mut long = profile();
        long.layer_seconds = 4.0e-3;
        assert!(tensor_parallel_viable(rdma(), long));
    }

    #[test]
    fn jeden_nod_daje_jeden_etap_tensor_parallel() {
        let caps = [cap(336e9, 1.1e12, 16), cap(735e9, 9.7e12, 20)];
        let topology = Topology::single_node(&caps, p2p()).unwrap();
        let plan = plan_execution(&topology, profile(), WorkKind::MemoryBound).unwrap();
        assert!(plan.is_pure_tensor_parallel());
        assert_eq!(plan.stages[0].workers, vec![0, 1]);
        assert_eq!(plan.stages[0].layers, 65);
    }

    #[test]
    fn dwa_nody_po_ethernecie_daja_pipeline() {
        let caps = [cap(700e9, 9.0e12, 20), cap(700e9, 9.0e12, 20)];
        let workers = vec![
            Worker {
                id: WorkerId {
                    node: NodeId(0),
                    device: 0,
                },
                capability: caps[0],
            },
            Worker {
                id: WorkerId {
                    node: NodeId(1),
                    device: 0,
                },
                capability: caps[1],
            },
        ];
        let links = vec![
            Link::same_device(),
            ethernet(),
            ethernet(),
            Link::same_device(),
        ];
        let topology = Topology::new(workers, links).unwrap();
        let plan = plan_execution(&topology, profile(), WorkKind::MemoryBound).unwrap();
        assert!(plan.is_pure_pipeline());
        assert_eq!(plan.stages.len(), 2);
        // Karty równe, więc warstwy dzielą się po połowie i suma się zgadza.
        assert_eq!(plan.stages[0].layers + plan.stages[1].layers, 65);
        assert_eq!(plan.stages[1].first_layer, plan.stages[0].layers);
    }

    #[test]
    fn dwa_nody_po_dwie_karty_daja_tp_w_nodzie_i_pp_miedzy() {
        // Dokładnie układ, o który chodzi: nody mogą mieć wiele kart.
        let c = cap(700e9, 9.0e12, 20);
        let workers: Vec<Worker> = (0..4)
            .map(|i| Worker {
                id: WorkerId {
                    node: NodeId(i as u32 / 2),
                    device: i % 2,
                },
                capability: c,
            })
            .collect();
        let mut links = Vec::new();
        for a in 0..4 {
            for b in 0..4 {
                links.push(if a == b {
                    Link::same_device()
                } else if a / 2 == b / 2 {
                    p2p()
                } else {
                    ethernet()
                });
            }
        }
        let topology = Topology::new(workers, links).unwrap();
        let plan = plan_execution(&topology, profile(), WorkKind::MemoryBound).unwrap();
        assert_eq!(plan.stages.len(), 2);
        assert_eq!(plan.stages[0].workers, vec![0, 1]);
        assert_eq!(plan.stages[1].workers, vec![2, 3]);
        assert_eq!(plan.stages[0].layers + plan.stages[1].layers, 65);
    }

    #[test]
    fn niespojne_lacza_sa_bledem_a_nie_cichym_planem() {
        // A-B szybkie, B-C szybkie, A-C wolne. Przechodniość dałaby grupę
        // {A,B,C}, w której para A-C nie uniesie wymiany.
        let c = cap(700e9, 9.0e12, 20);
        let workers: Vec<Worker> = (0..3)
            .map(|i| Worker {
                id: WorkerId {
                    node: NodeId(0),
                    device: i,
                },
                capability: c,
            })
            .collect();
        let mut links = vec![Link::same_device(); 9];
        for (a, b) in [(0, 1), (1, 0), (1, 2), (2, 1)] {
            links[a * 3 + b] = p2p();
        }
        for (a, b) in [(0, 2), (2, 0)] {
            links[a * 3 + b] = ethernet();
        }
        let topology = Topology::new(workers, links).unwrap();
        let error = plan_execution(&topology, profile(), WorkKind::MemoryBound).unwrap_err();
        assert!(format!("{error}").contains("niespójne"));
    }

    #[test]
    fn mocniejszy_etap_dostaje_wiecej_warstw() {
        let workers = vec![
            Worker {
                id: WorkerId {
                    node: NodeId(0),
                    device: 0,
                },
                capability: cap(300e9, 3.0e12, 20),
            },
            Worker {
                id: WorkerId {
                    node: NodeId(1),
                    device: 0,
                },
                capability: cap(900e9, 9.0e12, 20),
            },
        ];
        let links = vec![
            Link::same_device(),
            ethernet(),
            ethernet(),
            Link::same_device(),
        ];
        let topology = Topology::new(workers, links).unwrap();
        let plan = plan_execution(&topology, profile(), WorkKind::MemoryBound).unwrap();
        assert_eq!(plan.stages.len(), 2);
        assert!(plan.stages[1].layers > plan.stages[0].layers);
        assert_eq!(plan.stages[0].layers + plan.stages[1].layers, 65);
    }

    #[test]
    fn podzial_wierszy_w_etapie_uzywa_zmierzonej_mocy() {
        let caps = [cap(336e9, 1.1e12, 16), cap(735e9, 9.7e12, 20)];
        let topology = Topology::single_node(&caps, p2p()).unwrap();
        let plan = plan_execution(&topology, profile(), WorkKind::MemoryBound).unwrap();
        let rows = plan_stage_rows(&topology, &plan.stages[0], 4096, WorkKind::MemoryBound, 0)
            .unwrap();
        assert_eq!(rows.total(), 4096);
        assert!(rows.rows[1] > rows.rows[0]);
    }

    #[test]
    fn ta_sama_karta_ma_zerowy_koszt_wymiany() {
        assert_eq!(Link::same_device().exchange_seconds(1 << 20), 0.0);
    }

    #[test]
    fn martwe_lacze_jest_nieskonczenie_drogie() {
        let dead = Link {
            class: LinkClass::Ethernet,
            bytes_per_s: 0.0,
            latency_s: 0.0,
        };
        assert!(dead.exchange_seconds(1024).is_infinite());
        assert!(!tensor_parallel_viable(dead, profile()));
    }
}
