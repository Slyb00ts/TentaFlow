// ===== File: lib.rs — forge-graph: operacje jako dane =====
//
// Model to KOLEJNOŚĆ OPERACJI. Ten crate jest tą kolejnością zapisaną jako typ,
// a nie jako ciąg wywołań na jakimś urządzeniu.
//
// Różnica nie jest kosmetyczna. Gdy operacje są wywołaniami traitu, model musi
// znać wykonawcę, więc zależy od warstwy sprzętowej, więc jest modelem DLA tego
// sprzętu — i następny sprzęt dostaje własny model. Tak w tym repo powstały
// dwa pliki opisujące tę samą kolejność warstw: 2822 linie `forge-engine`
// dla CUDA i 1096 linii `forge-model` dla Metalu (docs/PRZEGLAD_UKLADU.md).
// Drugi z nich jest już wykonawcą i modelem osobno; pierwszy czeka.
//
// Gdy operacje są DANYMI, model nie ma czym nazwać bufora. Granica sprzętowa
// przestaje być regułą, której trzeba pilnować, a staje się faktem wynikającym
// z typów. I ciąg operacji da się przepisać przed wykonaniem — złączyć, zmienić
// kolejność, dobrać wariant — bez dotykania modeli, co jest długiem D1 planu
// naprawy.
//
// Ten crate NIE WIE o HAL i nie ma prawa się dowiedzieć.

use std::sync::Arc;

pub mod fuse;

use forge_formats::affine::AffineTriple;
use forge_types::{DType, DenseShape, ForgeError, QuantKind, Result};

/// Waga, którą wykonawca wgrał i trzyma.
///
/// Nieprzezroczysta celowo: model wie, która waga pełni którą rolę, a nie z
/// czego jest zrobiona. Ile ma bitów, jaką grupę i w czym trzyma skale — to
/// pytania do tego, kto ją mnoży. Model, który nie może ich zadać, nie może
/// przypadkiem zostać napisany pod jeden backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightId(pub u32);

/// Bufor roboczy, nazwany tym, co model w nim trzyma.
///
/// Typ zapisu NIE jest tu wymieniony celowo: wynika ze slotu, a nie z decyzji
/// modelu. Aktywacje idące dalej w mnożenia są półprecyzyjne, a te wracające do
/// strumienia rezydualnego albo do wyboru tokenu — pojedynczej. Model, który
/// mógłby to ustawić, mógłby to ustawić źle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Strumień rezydualny.
    Hidden,
    /// Wyjście bieżącej normalizacji.
    Norm,
    Query,
    Key,
    Value,
    /// Wyjście uwagi, przed projekcją wyjściową.
    Attn,
    /// The output gate of attention, when the architecture has one.
    ///
    /// A slot of its own rather than `Gate`, which is the feed-forward's and is
    /// as wide as `inter`. These two widths have no reason to agree — in
    /// Qwen3.6 the attention gate is 4096 and `inter` is 512 — so sharing the
    /// slot would silently write past the shorter of them.
    AttnGate,
    /// Wynik projekcji, dodawany z powrotem do strumienia.
    Proj,
    Gate,
    Up,
    /// Bramka i „up" złączone aktywacją.
    Activated,
    /// Logity ostatniego tokenu kafla.
    Logits,
}

/// Jedna sekwencja niesiona przez krok: gdzie trzyma swój kontekst i od której
/// pozycji go dokłada.
///
/// `slot` jest tożsamością sekwencji dla wykonawcy, a nie adresem: jak ten slot
/// wygląda w pamięci — jedna ciągła połać czy lista stron — jest sprawą tego,
/// kto trzyma cache. Model wie tylko, że dwie sekwencje o różnych slotach się
/// nie mieszają.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lane {
    pub slot: u32,
    /// Pozycja PIERWSZEGO tokenu tego kroku w tej sekwencji.
    pub pos: u32,
}

/// Sekwencje niesione przez jeden krok i ile tokenów niesie każda z nich.
///
/// JEDNA lista dla wszystkich operacji kroku, a nie liczba wierszy w jednych i
/// pozycje w drugich. Rozjazd między „ile wierszy mnoży projekcja" a „ile
/// lane'ów czyta uwaga" nie jest błędem kompilacji — jest cudzym kontekstem w
/// wyniku, czyli płynnym, złym tekstem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    lanes: Arc<[Lane]>,
    tokens: u32,
}

impl Step {
    /// Tyle samo tokenów w każdym lane. Krok mieszający prefill jednej
    /// sekwencji z dekodowaniem innych to osobna rzecz i osobne słownictwo —
    /// dopisanie go tutaj przez „długość na lane" pozwoliłoby modelowi opisać
    /// kształt, którego żaden kernel nie liczy.
    pub fn new(lanes: impl Into<Arc<[Lane]>>, tokens: u32) -> Result<Self> {
        let lanes = lanes.into();
        if lanes.is_empty() || tokens == 0 {
            return Err(ForgeError::Format(format!(
                "krok {} lane'ów po {tokens} tokenów jest pusty",
                lanes.len()
            )));
        }
        // Dwa lane'y w jednym slocie zapisałyby ten sam cache dwa razy w jednym
        // kroku, a wynik zależałby od kolejności bloków w kernelu.
        for (i, lane) in lanes.iter().enumerate() {
            if lanes[..i].iter().any(|other| other.slot == lane.slot) {
                return Err(ForgeError::Format(format!(
                    "slot {} występuje w kroku dwa razy",
                    lane.slot
                )));
            }
        }
        Ok(Self { lanes, tokens })
    }

    /// Jedna sekwencja — prefill i każdy przebieg, który nie jest wsadem.
    pub fn single(slot: u32, pos: u32, tokens: u32) -> Result<Self> {
        Self::new(vec![Lane { slot, pos }], tokens)
    }

    pub fn lanes(&self) -> &[Lane] {
        &self.lanes
    }

    pub fn tokens(&self) -> u32 {
        self.tokens
    }

    /// Wierszy aktywacji, lane po lanie. To jest ta liczba, którą dostają
    /// wszystkie operacje nieznające kontekstu.
    pub fn rows(&self) -> u32 {
        self.lanes.len() as u32 * self.tokens
    }
}

/// The expert every token passes through, alongside the ones it was routed to.
///
/// One struct rather than four more optional fields on `MoeFfn`, because the
/// four travel together: a block has this expert or it has not, and separate
/// options would let a model describe half of one.
///
/// It is NOT a fifth routed expert. Its output is added on top of the routed
/// sum, and its own gate is per token rather than per selection — so a mixture
/// computed without it is a different model, one that still speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shared {
    pub gate: WeightId,
    pub up: WeightId,
    pub down: WeightId,
    /// A single row whose logit, through a sigmoid, scales this expert's
    /// output for THIS token.
    ///
    /// Required rather than optional, and that is a decision. An architecture
    /// whose shared expert has no gate means weight 1.0 — which is not the
    /// same statement as having no shared expert, and the two would be one
    /// `None` here. Such a checkpoint is refused at load instead, until there
    /// is one to measure.
    pub router: WeightId,
}

/// Jedna operacja architektury gęstej.
///
/// Celowo wąskie i celowo nieogólne: to słownictwo JEDNEJ rodziny architektur,
/// a nie lista instrukcji. Szerszy zestaw pozwalałby modelowi wyrazić rzeczy,
/// których żaden backend nie liczy, a kompilator by tego nie powiedział.
///
/// Każda operacja niesie ten sam `Step`, więc wykonawca nigdy nie musi zgadywać,
/// ile wierszy ma przed sobą ani czyje one są.
#[derive(Debug, Clone)]
pub enum Op {
    /// Osadzenia tokenów kroku trafiają do `Act::Hidden`, lane po lanie.
    Embed {
        table: WeightId,
        tokens: Vec<u32>,
        step: Step,
    },
    RmsNorm {
        out: Act,
        x: Act,
        w: WeightId,
        step: Step,
    },
    /// `out = x * wagaᵀ`. Formę wybiera wykonawca; model nie ma tu zdania.
    MatMul {
        out: Act,
        w: WeightId,
        x: Act,
        step: Step,
    },
    /// RMS normalization SEPARATELY FOR EACH HEAD, with a learned weight of
    /// width `head_dim`, in place.
    ///
    /// A separate operation rather than a width added to `RmsNorm`, and that is
    /// a decision rather than a convenience. `RmsNorm` normalizes the residual
    /// stream and takes its width from the shape, so the model cannot state it
    /// wrongly. Were it to accept an arbitrary width, the model could describe
    /// a row split no kernel computes — which is the class of mistake this
    /// vocabulary exists to keep unrepresentable.
    ///
    /// `heads` is carried because Q and K DIFFER: the Qwen3 family normalizes
    /// 32 query heads and 4 KV heads within the same step.
    HeadNorm {
        act: Act,
        w: WeightId,
        heads: u32,
        step: Step,
    },
    Rope {
        act: Act,
        heads: u32,
        step: Step,
    },
    /// Dopisuje klucz i wartość tego kroku do cache'u warstwy.
    KvAppend {
        layer: usize,
        step: Step,
    },
    Attention {
        layer: usize,
        step: Step,
    },
    /// `Activated = silu(Gate) * Up`.
    SiluMul {
        step: Step,
    },
    /// `act *= sigmoid(gate)`, over the width of the attention output.
    ///
    /// The output gate of the Qwen3.5/3.6 attention block, which stores its
    /// query projection at twice the width and uses the second half of every
    /// head to gate what attention returns. Applied AFTER attention and before
    /// the output projection — a gate folded in earlier would scale the query
    /// instead of the answer, which is fluent and wrong.
    ///
    /// The width comes from the shape, like `SiluMul`'s does, so the model
    /// cannot name one no kernel computes.
    SigmoidMul {
        act: Act,
        gate: Act,
        step: Step,
    },
    /// The whole mixture-of-experts feed-forward block as one operation:
    /// routing, selection, the SwiGLU of the chosen experts, and their weighted
    /// accumulation.
    ///
    /// ONE operation rather than five, and the decision is the same one paging
    /// settled for `Attention`. Which expert computes is DATA produced on the
    /// device — a model that named those ids would have to read them back,
    /// buying back exactly the host round-trip the `_gidx` kernels exist to
    /// avoid. Expert residency is the same problem as cache pages, memory that
    /// does not hold everything, so it belongs to whoever holds it.
    ///
    /// The cost is that a pass cannot see inside the block. Measured: fusion at
    /// decode is worth 0.8%, because the whole matrix is read either way.
    ///
    /// The expert stacks are FLAT, `[experts * inter, hidden]`. The source
    /// keeps them three-dimensional, but an expert's rows are contiguous and
    /// the executor addresses them as a row window regardless.
    MoeFfn {
        out: Act,
        x: Act,
        /// The gate, `[experts, hidden]`: its output both selects and weights.
        router: WeightId,
        gate: WeightId,
        up: WeightId,
        down: WeightId,
        experts: u32,
        /// How many experts compute one token.
        top_k: u32,
        /// Whether the selected weights are renormalized to sum to one. A
        /// property of the architecture: OLMoE does not, the Qwen family does.
        norm_topk: bool,
        /// The always-on expert, for the architectures that have one.
        shared: Option<Shared>,
        step: Step,
    },
    /// RMSNorm followed by a projection, fused for decode-capable executors.
    FusedNormMatMul {
        out: Act,
        w: WeightId,
        norm_w: WeightId,
        x: Act,
        step: Step,
    },
    /// Projection followed by adding its result to the hidden stream.
    FusedMatMulResidual {
        w: WeightId,
        x: Act,
        step: Step,
    },
    /// `Hidden += src`.
    Residual {
        src: Act,
        step: Step,
    },
    /// Głowa wyjściowa, wyłącznie dla OSTATNIEGO tokenu KAŻDEGO lane'a —
    /// pozostałe wiersze służą tylko zapełnieniu cache'u, a ta macierz jest
    /// największa w modelu. Wynik to `[lane'y, słownik]`.
    LogitsOfLast {
        w: WeightId,
        x: Act,
        step: Step,
    },
}

/// Maszyna, która potrafi wykonać te operacje.
///
/// Odczyty i synchronizacja są osobno, a nie jako `Op`, bo nie są częścią
/// obliczenia — są jego przerwaniem. Pass przestawiający operacje musi wiedzieć,
/// że w tym miejscu nic nie wolno przestawić.
pub trait Executor {
    fn run(&self, op: &Op) -> Result<()>;

    /// Czeka na wszystko, co zostało zlecone.
    fn sync(&self) -> Result<()>;

    /// Odczytuje slot na hosta. `len` w elementach.
    ///
    /// Typ zapisu slotu jest sprawą wykonawcy, więc rozszerzenie do `f32` też:
    /// model, który musiałby wiedzieć, że akurat ten bufor jest półprecyzyjny,
    /// byłby modelem dla tego wykonawcy.
    fn read(&self, act: Act, len: usize) -> Result<Vec<f32>>;

    /// Zachłanny wybór na lane, policzony tam, gdzie logity już są.
    fn argmax(&self, act: Act, lanes: usize) -> Result<Vec<u32>>;

    /// Ile tokenów zmieści JEDNA sekwencja.
    fn seq_cap(&self) -> u32;

    /// Kafel, w jakim wykonawca chce dostawać tokeny.
    fn tile(&self) -> Tile;
}

/// Ile tokenów naraz i w jakiej wielokrotności.
///
/// Obie liczby są własnością WYKONAWCY, nie modelu: pierwsza wynika z tego, ile
/// aktywacji zmieścił, druga z geometrii kafla jego kerneli. Model, który by je
/// znał na stałe, znałby jeden backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    pub max_tokens: u32,
    /// Ile sekwencji naraz. Też własność wykonawcy: tyle, ile zmieścił slotów
    /// cache'u i wierszy aktywacji.
    ///
    /// Nie mnoży się swobodnie przez `max_tokens`: wykonawca może dodatkowo
    /// ograniczać ILOCZYN, bo to on jest liczbą wierszy scratcha. Krok ponad tę
    /// granicę odbija się przy uruchomieniu, z podaną liczbą wierszy.
    pub max_lanes: u32,
    /// Wielokrotność, do której warto wyrównać podział promptu. Prompt o jeden
    /// token dłuższy od niej każe policzyć następny kafel prawie pusty —
    /// zmierzone 5227 us/token przy 64 tokenach i 9318 przy 65.
    pub align: u32,
}

/// Wszystko, co wykonawca musi wiedzieć, ZANIM zobaczy pierwszą wagę.
///
/// Dwa typy, a nie jeden, bo to dwie różne rzeczy: parametry kwantyzacji i wagi
/// normalizacji. W MLX oba są bf16, więc zlanie ich w jeden wyglądało na
/// uproszczenie — i dawało poprawnie wyglądający, zły tekst dla każdego źródła,
/// które trzyma normy inaczej niż skale.
#[derive(Debug, Clone, Copy)]
pub struct ExecSpec {
    pub shape: DenseShape,
    /// Typ, w którym leżą skale i przesunięcia kwantyzacji.
    pub quant_params: DType,
    /// Typ wag normalizacji.
    pub norm_weights: DType,
}

/// Miejsce, w którym mieszkają wagi.
///
/// Model ładuje checkpoint, ale go nie TRZYMA: oddaje każdą wagę tutaj i
/// dostaje w zamian identyfikator. Dzięki temu model nie ma w sobie ani jednego
/// bufora urządzenia — a to jest jedyny powód, dla którego ten sam model liczy
/// się na dwóch kartach bez drugiej kopii.
/// Oba przyjmują wagę NA WŁASNOŚĆ, bo dla części wykonawców to jest ich
/// docelowe miejsce — a przekazanie przez referencję kazałoby im skopiować
/// każdą wagę modelu, czyli drugi raz zająć tyle pamięci, ile checkpoint waży.
pub trait WeightStore {
    /// Waga kwantyzowana, w postaci ŹRÓDŁOWEJ. Wykonawca SPRAWDZA, czy pasuje do
    /// tego, w czym skompilował kernele — zamiast ufać, że wołający pamiętał.
    fn put_quant(&mut self, w: QuantWeight) -> Result<WeightId>;

    /// Waga niekwantyzowana, w bajtach źródła.
    fn put_plain(&mut self, bytes: Vec<u8>) -> Result<WeightId>;
}

/// Płaszczyzny bajtów, z których składa się waga.
///
/// Formaty blokowe GGUF-a mają JEDNĄ — skale siedzą wewnątrz bloku. NVFP4 ma
/// kody, osobny bufor skal i skalar na cały tensor; FP8 ze skalą wierszową ma
/// bajty i wektor skal. Dopóki typ mówił „waga to jedna bryła bajtów", te trzy
/// nie mogły wejść do wspólnej tabeli i musiały mieć własne gałęzie — czyli
/// dokładnie te odnogi, których ten układ ma nie mieć.
///
/// Jedna płaszczyzna jest tu przypadkiem ZDEGENEROWANYM, a nie regułą.
#[derive(Debug, Default)]
pub struct Planes {
    /// Kody wagi w układzie źródła. Zawsze obecne.
    pub codes: Vec<u8>,
    /// Skale, gdy format trzyma je poza kodami.
    pub scales: Option<Vec<u8>>,
    /// Skalar całego tensora, gdy format go ma.
    pub global: Option<f32>,
}

/// Gdzie leżą bajty jednej kwantyzacji.
///
/// NVFP4 przychodzi w dwóch układach — blokach GGUF-a i trzech tensorach
/// compressed-tensors — a one kodują TE SAME LICZBY co do bitu (pilnuje tego
/// `the_gguf_repack_decodes_to_the_same_numbers` w `forge-formats`). Nazwanie
/// ich dwiema kwantyzacjami mówiłoby nieprawdę i pociągnęło dwie tabele
/// formatów; różni się miejsce bajtów, a nie format.
///
/// Dzięki temu przepakowanie przy wczytaniu jest DECYZJĄ, a nie przymusem:
/// wykonawca może wziąć układ źródła albo poprosić o przepisany, i rozstrzyga
/// to pomiar, a nie kształt typu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// Bloki formatu, wiersz po wierszu — tak leży GGUF i każda kwantyzacja,
    /// która trzyma skale wewnątrz bloku.
    #[default]
    Blocks,
    /// Kody i skale w osobnych tensorach — układ compressed-tensors.
    Split,
}

/// Waga kwantyzowana w postaci, w jakiej oddało ją źródło.
///
/// Przepisanie na postać, której chcą kernele, należy do WYKONAWCY, a nie do
/// modelu. To nie jest kosmetyka: kernele Metalowe indeksują trzy osobne
/// tablice, a kernele CUDA czytają bloki GGUF-a w oryginalnym układzie, więc
/// model przepisujący wszystko „na jedno" musiałby wybrać jedną z tych dwóch
/// stron. Wybór na rzecz postaci afinicznej jest przy tym STRATNY dla Q6_K,
/// bo sześciu bitów nie da się włożyć w cztery.
#[derive(Debug)]
pub struct PackedWeight {
    pub planes: Planes,
    pub quant: QuantKind,
    /// Gdzie leżą bajty tej kwantyzacji.
    pub layout: Layout,
    /// Typ zapisu kodów. Dla formatów blokowych to bajty i nikt go nie czyta;
    /// dla wagi NIEKWANTYZOWANEJ to jest cały jej format, bo `QuantKind::None`
    /// samo w sobie nie mówi, czy to f16 czy bf16.
    pub dtype: DType,
    pub rows: usize,
    pub cols: usize,
}

impl PackedWeight {
    /// Skale jako osobna płaszczyzna — dla formatów, które ich tam szukają.
    ///
    /// Wiersz tabeli DEKLARUJE, czego potrzebuje, więc waga bez tej płaszczyzny
    /// odbija się przy wgraniu, a nie przy mnożeniu. Odwrotna kolejność znaczy
    /// kernel czytający cudzą pamięć albo zera.
    pub fn scales(&self) -> Result<&[u8]> {
        self.planes.scales.as_deref().ok_or_else(|| {
            ForgeError::Format(format!(
                "{:?} wymaga osobnej płaszczyzny skal, a źródło jej nie oddało",
                self.quant
            ))
        })
    }

    /// Skalar całego tensora — tak samo deklarowany, tak samo sprawdzany.
    pub fn global(&self) -> Result<f32> {
        self.planes.global.ok_or_else(|| {
            ForgeError::Format(format!(
                "{:?} wymaga skalara tensora, a źródło go nie oddało",
                self.quant
            ))
        })
    }
}

pub enum QuantWeight {
    /// Kody i, gdy format ich potrzebuje, osobne skale.
    Packed(PackedWeight),
    /// Źródło trzyma postać afiniczną NATYWNIE i nie ma innej — tak wygląda
    /// eksport MLX, którego skale są w bf16. Przepuszczanie go przez format
    /// pośredni po to, żeby wszystko wyglądało jednakowo, zwężało je do f16.
    Affine(AffineTriple),
}

impl QuantWeight {
    /// Kształt, niezależnie od postaci — do sprawdzenia wobec tego, czego
    /// oczekuje architektura.
    pub fn shape(&self) -> (usize, usize) {
        match self {
            Self::Packed(p) => (p.rows, p.cols),
            Self::Affine(t) => (t.rows, t.cols),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Krok, w którym ten sam slot występuje dwa razy, musi być błędem.
    ///
    /// Oba lane'y dopisałyby do jednego cache'u w jednym kroku, więc wynik
    /// zależałby od kolejności bloków w kernelu — a to jest niepowtarzalna
    /// odpowiedź, nie awaria.
    #[test]
    fn one_slot_cannot_be_two_lanes_of_one_step() {
        let twice = vec![Lane { slot: 1, pos: 0 }, Lane { slot: 1, pos: 7 }];
        assert!(Step::new(twice, 1).is_err(), "powtórzony slot przeszedł");

        let apart = vec![Lane { slot: 0, pos: 3 }, Lane { slot: 1, pos: 7 }];
        let step = Step::new(apart, 2).expect("dwa różne sloty");
        assert_eq!(step.rows(), 4, "wiersze to lane'y razy tokeny");
    }

    /// Waga bez płaszczyzny, której format żąda, ma się zatrzymać przy pytaniu
    /// o nią — a nie policzyć bez niej.
    ///
    /// To jest cały mechanizm, przez który NVFP4 i FP8 mieszczą się w tej samej
    /// tabeli co Q4_K: wiersz DEKLARUJE, czego potrzebuje, zamiast każdy
    /// wykonawca sprawdzał, które pola są wypełnione.
    #[test]
    fn a_missing_plane_is_refused_where_it_is_asked_for() {
        let one_blob = PackedWeight {
            planes: Planes {
                codes: vec![0u8; 144],
                ..Planes::default()
            },
            quant: QuantKind::Q4K,
            layout: Layout::Blocks,
            dtype: DType::U8,
            rows: 1,
            cols: 256,
        };
        assert!(one_blob.scales().is_err(), "brakująca płaszczyzna przeszła");
        assert!(one_blob.global().is_err(), "brakujący skalar przeszedł");

        let three = PackedWeight {
            planes: Planes {
                codes: vec![0u8; 128],
                scales: Some(vec![0u8; 16]),
                global: Some(0.5),
            },
            quant: QuantKind::NVFP4Gguf,
            layout: Layout::Blocks,
            dtype: DType::U8,
            rows: 1,
            cols: 256,
        };
        assert_eq!(three.scales().expect("skale").len(), 16);
        assert_eq!(three.global().expect("skalar"), 0.5);
    }

    /// Pusty krok nie ma czego policzyć, a każdy kernel dostałby zero wierszy
    /// i cicho nic nie zrobił.
    #[test]
    fn an_empty_step_is_refused() {
        assert!(Step::new(vec![], 1).is_err(), "krok bez lane'ów przeszedł");
        assert!(Step::single(0, 0, 0).is_err(), "krok bez tokenów przeszedł");
    }
}
