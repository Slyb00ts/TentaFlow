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

use forge_formats::affine::AffineTriple;
use forge_types::{DType, DenseShape, Result};

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
    /// Wynik projekcji, dodawany z powrotem do strumienia.
    Proj,
    Gate,
    Up,
    /// Bramka i „up" złączone aktywacją.
    Activated,
    /// Logity ostatniego tokenu kafla.
    Logits,
}

/// Jedna operacja architektury gęstej.
///
/// Celowo wąskie i celowo nieogólne: to słownictwo JEDNEJ rodziny architektur,
/// a nie lista instrukcji. Szerszy zestaw pozwalałby modelowi wyrazić rzeczy,
/// których żaden backend nie liczy, a kompilator by tego nie powiedział.
#[derive(Debug, Clone)]
pub enum Op {
    /// Osadzenia tokenów kafla trafiają do `Act::Hidden`.
    Embed { table: WeightId, tokens: Vec<u32> },
    RmsNorm { out: Act, x: Act, w: WeightId, tokens: u32 },
    /// `out = x * wagaᵀ`. Formę wybiera wykonawca; model nie ma tu zdania.
    MatMul { out: Act, w: WeightId, x: Act, tokens: u32 },
    Rope { act: Act, heads: u32, pos: u32, tokens: u32 },
    /// Dopisuje klucz i wartość tego kafla do cache'u warstwy.
    KvAppend { layer: usize, pos: u32, tokens: u32 },
    Attention { layer: usize, seq: u32, tokens: u32 },
    /// `Activated = silu(Gate) * Up`.
    SiluMul { tokens: u32 },
    /// `Hidden += src`.
    Residual { src: Act, tokens: u32 },
    /// Głowa wyjściowa, wyłącznie dla OSTATNIEGO tokenu kafla — pozostałe
    /// wiersze służą tylko zapełnieniu cache'u, a ta macierz jest największa
    /// w modelu.
    LogitsOfLast { w: WeightId, x: Act, tokens: u32 },
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

    /// Zachłanny wybór, policzony tam, gdzie logity już są.
    fn argmax(&self, act: Act) -> Result<u32>;

    /// Ile tokenów zmieści cache klucza i wartości.
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
pub trait WeightStore {
    /// Waga kwantyzowana afinicznie. Wykonawca SPRAWDZA, czy pasuje do tego, w
    /// czym skompilował kernele — zamiast ufać, że wołający pamiętał.
    fn put_affine(&mut self, t: &AffineTriple) -> Result<WeightId>;

    /// Waga niekwantyzowana, w bajtach źródła.
    fn put_plain(&mut self, bytes: &[u8]) -> Result<WeightId>;
}
