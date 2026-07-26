// ===== File: moe_residency.rs — per-expert residency of routed MoE weights =====
//
// Sklejony stos ekspertów `[n_experts * rows, cols]` jest tu rozbity na osobne
// uchwyty — jeden na eksperta. To warunek konieczny dla rezydencji warstwowej:
// dopiero gdy ekspert jest samodzielną alokacją, może leżeć w VRAM albo w
// przypiętej pamięci hosta niezależnie od sąsiadów i wędrować między nimi.
//
// Kernele `_gidx` nie dostają już bazy stosu i skoku wiersza, tylko tablicę
// wskaźników `table[e]` — bazę eksperta `e`. Wybór nadal zapada NA URZĄDZENIU
// (z `ids[sel]` routera), więc ścieżka dekodowania zachowuje zero odczytów na
// hosta. Kosztem jest jeden zależny odczyt na blok, jednolity dla całego bloku.

use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_types::{ForgeError, MemKind, Result};

use crate::weights::DevWeight;

/// Warstwa pamięci, w której fizycznie leży ekspert.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExpertTier {
    /// Pamięć urządzenia.
    Vram,
    /// Przypięta pamięć hosta, czytana przez kernel po PCIe (UVA).
    Host,
}

/// Eksperci jednej projekcji jednej warstwy MoE, każdy jako osobny uchwyt, plus
/// rezydentna na urządzeniu tablica ich wskaźników bazowych.
pub struct ExpertStack {
    experts: Vec<DevWeight>,
    tiers: Vec<ExpertTier>,
    table: DevBuffer,
    rows_per_expert: usize,
    cols: usize,
    bytes_per_expert: usize,
}

impl ExpertStack {
    /// Rozbija sklejony stos na eksperty i buduje tablicę wskaźników.
    ///
    /// Rozmieszczenie wynika z tego, co zwróci `device.alloc`: przy urządzeniu
    /// owiniętym w `TieredWeightDevice` eksperty lądują w VRAM aż do jego
    /// wyczerpania, a dalsze w przypiętej pamięci hosta. Każdy ekspert ma tu
    /// ten sam rozmiar, więc późniejsza migracja może zamieniać zawartość
    /// dwóch slotów bez żadnej realokacji.
    pub fn upload(
        device: &dyn Device,
        experts: Vec<DevWeight>,
        rows_per_expert: usize,
        cols: usize,
    ) -> Result<Self> {
        if experts.is_empty() {
            return Err(ForgeError::Format(
                "stos ekspertów MoE jest pusty".to_string(),
            ));
        }
        let expert_buffer = |w: &DevWeight| -> Result<DevBuffer> {
            w.buffer().cloned().ok_or_else(|| {
                ForgeError::Unsupported(
                    "rezydencja ekspertów wymaga wagi o jednym buforze bajtów".into(),
                )
            })
        };
        let bytes_per_expert = expert_buffer(&experts[0])?.len();
        let mut tiers = Vec::with_capacity(experts.len());
        let mut addrs = Vec::with_capacity(experts.len());
        for expert in &experts {
            let buf = expert_buffer(expert)?;
            if buf.len() != bytes_per_expert {
                return Err(ForgeError::Format(format!(
                    "eksperci MoE różnią się rozmiarem: {} vs {bytes_per_expert} B",
                    buf.len()
                )));
            }
            tiers.push(match buf.kind() {
                MemKind::Device => ExpertTier::Vram,
                _ => ExpertTier::Host,
            });
            addrs.push(buf.device_ptr());
        }
        let table = device.alloc(addrs.len() * 8, MemKind::Device, Pool::Weights)?;
        device.write(bytemuck_u64(&addrs), &table, 0)?;
        Ok(Self {
            experts,
            tiers,
            table,
            rows_per_expert,
            cols,
            bytes_per_expert,
        })
    }

    pub fn n_experts(&self) -> usize {
        self.experts.len()
    }

    pub fn rows_per_expert(&self) -> usize {
        self.rows_per_expert
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn bytes_per_expert(&self) -> usize {
        self.bytes_per_expert
    }

    /// Waga pojedynczego eksperta — dla ścieżek, które znają wybór na hoście
    /// (prefill i kwantyzacje bez kernela `_gidx`).
    pub fn expert(&self, index: usize) -> Result<&DevWeight> {
        self.experts.get(index).ok_or_else(|| {
            ForgeError::Format(format!(
                "ekspert {index} poza zakresem {}",
                self.experts.len()
            ))
        })
    }

    /// Reprezentant stosu — do rozpoznania kwantyzacji i kształtu.
    pub fn representative(&self) -> &DevWeight {
        &self.experts[0]
    }

    /// Rezydentna na urządzeniu tablica `u64[n_experts]` z bazami ekspertów.
    pub fn table(&self) -> &DevBuffer {
        &self.table
    }

    pub fn tier(&self, index: usize) -> ExpertTier {
        self.tiers[index]
    }

    /// Zamienia miejscami zawartość eksperta z VRAM i eksperta z pamięci
    /// hosta, po czym przepisuje oba wpisy tablicy wskaźników.
    ///
    /// Zamiana zamiast realokacji jest tu celowa: pula wag to arena bump, która
    /// nie zwalnia pojedynczych bloków, a wszyscy eksperci jednej projekcji są
    /// równej wielkości. Inwentarz slotów jest więc stały przez całe życie
    /// modelu, a migracja to wyłącznie przesunięcie bajtów.
    ///
    /// Wywołujący MUSI mieć pewność, że urządzenie nie ma w locie pracy
    /// czytającej te wagi — kopie idą strumieniem, ale wpis tablicy jest
    /// zapisywany z hosta.
    pub fn swap_tiers(
        &mut self,
        device: &dyn Device,
        vram_index: usize,
        host_index: usize,
        scratch: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        if self.tiers[vram_index] != ExpertTier::Vram || self.tiers[host_index] != ExpertTier::Host {
            return Err(ForgeError::Kernel(format!(
                "zamiana warstw wymaga eksperta w VRAM i eksperta w hoście, dostano {:?}/{:?}",
                self.tiers[vram_index], self.tiers[host_index]
            )));
        }
        let bytes = self.bytes_per_expert;
        if scratch.len() < bytes {
            return Err(ForgeError::Kernel(format!(
                "bufor przesiadkowy migracji ma {} B, potrzeba {bytes}",
                scratch.len()
            )));
        }
        let in_vram = self.buffer(vram_index)?;
        let in_host = self.buffer(host_index)?;
        device.copy(&in_vram, 0, scratch, 0, bytes, stream)?;
        device.copy(&in_host, 0, &in_vram, 0, bytes, stream)?;
        device.copy(scratch, 0, &in_host, 0, bytes, stream)?;
        stream.synchronize()?;

        self.experts.swap(vram_index, host_index);
        self.tiers.swap(vram_index, host_index);
        for index in [vram_index, host_index] {
            let addr = self.buffer(index)?.device_ptr();
            device.write(&addr.to_le_bytes(), &self.table, index * 8)?;
        }
        Ok(())
    }

    fn buffer(&self, index: usize) -> Result<DevBuffer> {
        self.expert(index)?.buffer().cloned().ok_or_else(|| {
            ForgeError::Unsupported("rezydencja ekspertów wymaga wagi o jednym buforze".into())
        })
    }

    /// Liczba ekspertów w VRAM i w pamięci hosta.
    pub fn tier_counts(&self) -> (usize, usize) {
        let vram = self
            .tiers
            .iter()
            .filter(|t| **t == ExpertTier::Vram)
            .count();
        (vram, self.tiers.len() - vram)
    }
}

/// Liczniki wyboru ekspertów jednej warstwy MoE, zapisywane przez kernel
/// routera. Tylko one wiedzą, którzy eksperci są gorący.
pub struct ExpertUsage {
    counts: DevBuffer,
    n_experts: usize,
}

impl ExpertUsage {
    pub fn new(device: &dyn Device, n_experts: usize) -> Result<Self> {
        let counts = device.alloc(n_experts * 4, MemKind::Device, Pool::Weights)?;
        device.write(&vec![0u8; n_experts * 4], &counts, 0)?;
        Ok(Self { counts, n_experts })
    }

    pub fn counts(&self) -> &DevBuffer {
        &self.counts
    }

    /// Odczytuje liczniki i zeruje je, oddając okno od ostatniego wywołania.
    pub fn take(&self, device: &dyn Device) -> Result<Vec<u32>> {
        let mut bytes = vec![0u8; self.n_experts * 4];
        device.read(&self.counts, 0, &mut bytes)?;
        device.write(&vec![0u8; self.n_experts * 4], &self.counts, 0)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }
}

/// Adres jednej projekcji jednej warstwy MoE.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProjectionId {
    pub layer: usize,
    pub projection: Projection,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Projection {
    Gate,
    Up,
    Down,
}

/// Jedna zaplanowana migracja: ekspert `promote` ma trafić do VRAM w miejsce
/// eksperta `demote`.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    pub target: ProjectionId,
    pub promote: usize,
    pub demote: usize,
    /// Przewaga popularności kandydata nad rezydentem. Rundę ograniczamy
    /// globalnie, więc trzeba móc porównać zyski z różnych warstw.
    pub gain: f32,
}

/// Polityka rezydencji: utrzymuje wygładzoną popularność ekspertów i wskazuje,
/// które zamiany opłaca się wykonać.
///
/// Popularność liczona jest wykładniczo (EMA), bo rozkład trafień routera
/// dryfuje razem z treścią rozmowy — surowa suma od startu zamroziłaby układ na
/// pierwszym temacie. Zamiany są limitowane z dwóch stron: rzadkością rund i
/// twardym limitem na rundę, żeby ruch bajtów nie zjadł zysku z trafień.
pub struct ResidencyPolicy {
    /// Popularność [warstwa][ekspert].
    popularity: Vec<Vec<f32>>,
    decay: f32,
    /// Ile razy popularność kandydata musi przewyższyć obecnego rezydenta,
    /// żeby zamiana miała sens. Bez tego progu sąsiadujące wyniki wywołałyby
    /// wieczne przerzucanie tych samych dwóch ekspertów.
    hysteresis: f32,
    /// Twardy limit zamian na rundę, liczony PRZEZ CAŁY MODEL. Limit per
    /// projekcja przepuściłby przy 61 warstwach setki przeniesień naraz.
    max_migrations_per_round: usize,
}

impl ResidencyPolicy {
    pub fn new(n_layers: usize, n_experts: usize) -> Self {
        Self {
            popularity: vec![vec![0.0; n_experts]; n_layers],
            decay: 0.75,
            hysteresis: 1.25,
            max_migrations_per_round: 8,
        }
    }

    /// Wchłania okno liczników warstwy.
    pub fn observe(&mut self, layer: usize, counts: &[u32]) {
        let popularity = &mut self.popularity[layer];
        for (slot, &count) in popularity.iter_mut().zip(counts) {
            *slot = *slot * self.decay + count as f32;
        }
    }

    pub fn popularity(&self, layer: usize) -> &[f32] {
        &self.popularity[layer]
    }

    /// Zamiany opłacalne dla jednej projekcji: najpopularniejsi eksperci poza
    /// VRAM zastępują najmniej popularnych rezydentów, o ile przewaga
    /// przekracza histerezę. Liczba miejsc w VRAM jest zastana — polityka nie
    /// zmienia podziału pamięci, tylko to, kto go zajmuje.
    pub fn candidates(&self, target: ProjectionId, stack: &ExpertStack) -> Vec<Migration> {
        let popularity = &self.popularity[target.layer];
        let mut resident: Vec<usize> = Vec::new();
        let mut outside: Vec<usize> = Vec::new();
        for e in 0..stack.n_experts() {
            match stack.tier(e) {
                ExpertTier::Vram => resident.push(e),
                ExpertTier::Host => outside.push(e),
            }
        }
        if resident.is_empty() || outside.is_empty() {
            return Vec::new();
        }
        // Najsłabszy rezydent na początku, najsilniejszy kandydat na początku.
        resident.sort_by(|a, b| popularity[*a].total_cmp(&popularity[*b]));
        outside.sort_by(|a, b| popularity[*b].total_cmp(&popularity[*a]));

        let mut plan = Vec::new();
        for (&demote, &promote) in resident.iter().zip(outside.iter()) {
            if popularity[promote] <= popularity[demote] * self.hysteresis {
                break;
            }
            plan.push(Migration {
                target,
                promote,
                demote,
                gain: popularity[promote] - popularity[demote],
            });
        }
        plan
    }

    /// Wybiera z kandydatów całego modelu te zamiany, które zwracają najwięcej
    /// za przeniesiony bajt, i przycina rundę do limitu.
    pub fn select_round(&self, mut candidates: Vec<Migration>) -> Vec<Migration> {
        candidates.sort_by(|a, b| b.gain.total_cmp(&a.gain));
        candidates.truncate(self.max_migrations_per_round);
        candidates
    }
}

/// Ile tokenów dekodowania dzieli kolejne rundy przeglądu rezydencji.
pub const MOE_RESIDENCY_INTERVAL: usize = 128;

/// Stan rezydencji ekspertów żyjący razem z modelem.
pub struct MoeResidencyState {
    pub policy: ResidencyPolicy,
    /// Bufor przesiadkowy na jednego eksperta, wielkości największej projekcji.
    pub scratch: DevBuffer,
    pub tokens_since_round: usize,
}

impl MoeResidencyState {
    /// `None`, gdy model nie jest MoE albo wszyscy eksperci zmieścili się w
    /// VRAM: bez eksperta poza VRAM nie ma czego migrować, a sama runda
    /// kosztuje synchronizację.
    pub fn new(
        device: &dyn Device,
        n_layers: usize,
        moe_layers: &[MoeLayerView<'_>],
    ) -> Result<Option<Self>> {
        let mut spilled = false;
        let mut widest = 0usize;
        let mut n_experts = 0usize;
        for layer in moe_layers {
            for stack in layer.stacks() {
                let (_, host) = stack.tier_counts();
                spilled |= host > 0;
                widest = widest.max(stack.bytes_per_expert());
                n_experts = n_experts.max(stack.n_experts());
            }
        }
        if !spilled {
            return Ok(None);
        }
        Ok(Some(Self {
            policy: ResidencyPolicy::new(n_layers, n_experts),
            scratch: device.alloc(widest, MemKind::PinnedHost, Pool::Weights)?,
            tokens_since_round: 0,
        }))
    }
}

/// Trzy projekcje jednej warstwy MoE — na tyle, ile potrzebuje polityka.
pub struct MoeLayerView<'a> {
    pub gate: &'a ExpertStack,
    pub up: &'a ExpertStack,
    pub down: &'a ExpertStack,
}

impl<'a> MoeLayerView<'a> {
    fn stacks(&self) -> [&'a ExpertStack; 3] {
        [self.gate, self.up, self.down]
    }
}

/// Widok bajtowy na tablicę adresów bez dodatkowej zależności.
fn bytemuck_u64(addrs: &[u64]) -> &[u8] {
    // Bezpieczne: u64 nie ma niezainicjowanych bitów ani wymagań wyrównania
    // ostrzejszych niż źródłowy wycinek, a długość liczona jest z rozmiaru.
    unsafe { std::slice::from_raw_parts(addrs.as_ptr() as *const u8, std::mem::size_of_val(addrs)) }
}
