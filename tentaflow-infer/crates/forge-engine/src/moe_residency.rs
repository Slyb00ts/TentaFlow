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
//
// Trzy warstwy, dwa różne mechanizmy — i to rozróżnienie jest tu istotne:
//
//   VRAM  <-> RAM   przestawiane co kilkadziesiąt tokenów, według popularności.
//                   Obie warstwy są adresowalne przez kernel, więc ruch nigdy
//                   nie zmienia tego, co jest możliwe — przenosi tylko
//                   częściej używanych na szybsze miejsce.
//   RAM   <-> NVMe  stronicowanie NA ŻĄDANIE. Dysku kernel nie zaadresuje, więc
//                   trafienie w eksperta z dysku MUSI go najpierw ściągnąć, a to
//                   wymaga poznania wyboru routera na hoście. Warstwa z choćby
//                   jednym ekspertem na dysku traci więc ścieżkę bez odczytu
//                   wstecznego — to cena za to, że model w ogóle się mieści.
//
// Sloty są przydzielone raz i nigdy nie zmieniają adresu; migracja to wyłącznie
// przeniesienie bajtów i przepisanie wpisu tablicy. Pula wag jest areną bump,
// która nie zwalnia pojedynczych bloków, więc realokacja i tak nie byłaby
// możliwa.

use std::sync::Mutex;

use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_types::{ForgeError, MemKind, Result};

use crate::expert_spill::{ExpertSpill, SpillRegion, SpillTarget};
use crate::weights::DevWeight;

/// Warstwa pamięci, w której leżą bajty eksperta.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExpertTier {
    /// Pamięć urządzenia.
    Vram,
    /// Przypięta pamięć hosta, czytana przez kernel po PCIe (UVA).
    Host,
    /// Plik zrzutu; nieadresowalna dla kernela, wymaga ściągnięcia do slotu.
    Nvme,
}

/// Zmienny stan rozmieszczenia. Wydzielony za mutex, bo stronicowanie na
/// żądanie zachodzi w kroku dekodowania, który trzyma model tylko przez `&self`.
struct Placement {
    /// Ekspert -> slot, o ile jest rezydentny.
    slot_of: Vec<Option<usize>>,
    /// Slot -> ekspert, którego bajty w nim leżą.
    owner_of: Vec<usize>,
    /// Położenie eksperta w pliku zrzutu; `Some` dla każdego, który może zostać
    /// wyparty albo startuje poza pamięcią.
    spilled: Vec<Option<SpillRegion>>,
}

/// Eksperci jednej projekcji jednej warstwy MoE plus rezydentna na urządzeniu
/// tablica ich wskaźników bazowych.
pub struct ExpertStack {
    /// Sloty rezydentne. Same uchwyty są niezmienne przez całe życie modelu —
    /// zmienia się tylko to, czyje bajty w nich leżą.
    slots: Vec<DevWeight>,
    slot_tier: Vec<ExpertTier>,
    table: DevBuffer,
    placement: Mutex<Placement>,
    n_experts: usize,
    rows_per_expert: usize,
    cols: usize,
    bytes_per_expert: usize,
}

impl ExpertStack {
    /// Buduje stos z gotowych slotów.
    ///
    /// `resident[i]` to ekspert, którego bajty leżą w slocie `i`; `spilled`
    /// opisuje kopie na dysku. Rozmieszczenie między VRAM a hostem wynika z
    /// tego, co zwróciło `device.alloc` — przy urządzeniu owiniętym w
    /// `TieredWeightDevice` sloty lądują w VRAM aż do jego wyczerpania, a
    /// dalsze w przypiętej pamięci hosta.
    pub fn new(
        device: &dyn Device,
        slots: Vec<DevWeight>,
        resident: Vec<usize>,
        spilled: Vec<Option<SpillRegion>>,
        rows_per_expert: usize,
        cols: usize,
    ) -> Result<Self> {
        if slots.is_empty() {
            return Err(ForgeError::Format(
                "stos ekspertów MoE nie ma ani jednego slotu".to_string(),
            ));
        }
        if slots.len() != resident.len() {
            return Err(ForgeError::Format(format!(
                "{} slotów wobec {} przypisanych ekspertów",
                slots.len(),
                resident.len()
            )));
        }
        let n_experts = spilled.len();
        let bytes_per_expert = slot_buffer(&slots[0])?.len();
        let mut slot_tier = Vec::with_capacity(slots.len());
        for slot in &slots {
            let buf = slot_buffer(slot)?;
            if buf.len() != bytes_per_expert {
                return Err(ForgeError::Format(format!(
                    "sloty ekspertów różnią się rozmiarem: {} vs {bytes_per_expert} B",
                    buf.len()
                )));
            }
            slot_tier.push(match buf.kind() {
                MemKind::Device => ExpertTier::Vram,
                _ => ExpertTier::Host,
            });
        }

        let mut slot_of = vec![None; n_experts];
        for (slot, &expert) in resident.iter().enumerate() {
            if expert >= n_experts {
                return Err(ForgeError::Format(format!(
                    "slot {slot} przypisany do eksperta {expert} spoza {n_experts}"
                )));
            }
            if slot_of[expert].is_some() {
                return Err(ForgeError::Format(format!(
                    "ekspert {expert} przypisany do dwóch slotów"
                )));
            }
            slot_of[expert] = Some(slot);
        }
        for (expert, region) in spilled.iter().enumerate() {
            if slot_of[expert].is_none() && region.is_none() {
                return Err(ForgeError::Format(format!(
                    "ekspert {expert} nie jest ani rezydentny, ani zrzucony"
                )));
            }
        }

        let table = device.alloc(n_experts * 8, MemKind::Device, Pool::Weights)?;
        let stack = Self {
            slots,
            slot_tier,
            table,
            placement: Mutex::new(Placement {
                slot_of,
                owner_of: resident,
                spilled,
            }),
            n_experts,
            rows_per_expert,
            cols,
            bytes_per_expert,
        };
        stack.rewrite_table(device)?;
        Ok(stack)
    }

    /// Przepisuje całą tablicę wskaźników z bieżącego rozmieszczenia.
    /// Nierezydentny ekspert dostaje adres zerowy — kernel `_gidx` nigdy go nie
    /// zobaczy, bo warstwa z ekspertem na dysku nie używa tej ścieżki.
    fn rewrite_table(&self, device: &dyn Device) -> Result<()> {
        let placement = self.placement.lock().expect("rozmieszczenie zatrute");
        let mut addrs = vec![0u64; self.n_experts];
        for (expert, slot) in placement.slot_of.iter().enumerate() {
            if let Some(slot) = slot {
                addrs[expert] = slot_buffer(&self.slots[*slot])?.device_ptr();
            }
        }
        device.write(bytemuck::cast_slice(&addrs), &self.table, 0)
    }

    pub fn n_experts(&self) -> usize {
        self.n_experts
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

    /// Czy każdy ekspert ma swój slot. Tylko wtedy wolno użyć ścieżki `_gidx`,
    /// bo tylko wtedy każdy możliwy wybór routera jest adresowalny.
    pub fn fully_resident(&self) -> bool {
        self.placement
            .lock()
            .expect("rozmieszczenie zatrute")
            .slot_of
            .iter()
            .all(Option::is_some)
    }

    /// Waga eksperta — dla ścieżek, które znają wybór na hoście (prefill i
    /// kwantyzacje bez kernela `_gidx`). Błąd, gdy ekspert siedzi na dysku:
    /// wywołujący miał go wcześniej ściągnąć.
    pub fn expert(&self, index: usize) -> Result<&DevWeight> {
        let slot = self
            .placement
            .lock()
            .expect("rozmieszczenie zatrute")
            .slot_of
            .get(index)
            .copied()
            .flatten();
        match slot {
            Some(slot) => Ok(&self.slots[slot]),
            None => Err(ForgeError::Kernel(format!(
                "ekspert {index} nie jest rezydentny — brak wcześniejszego ściągnięcia"
            ))),
        }
    }

    /// Reprezentant stosu — do rozpoznania kwantyzacji i kształtu.
    pub fn representative(&self) -> &DevWeight {
        &self.slots[0]
    }

    /// Rezydentna na urządzeniu tablica `u64[n_experts]` z bazami ekspertów.
    pub fn table(&self) -> &DevBuffer {
        &self.table
    }

    pub fn tier(&self, expert: usize) -> ExpertTier {
        match self
            .placement
            .lock()
            .expect("rozmieszczenie zatrute")
            .slot_of[expert]
        {
            Some(slot) => self.slot_tier[slot],
            None => ExpertTier::Nvme,
        }
    }

    /// Ilu ekspertów siedzi w VRAM, w pamięci hosta i na dysku.
    pub fn tier_counts(&self) -> (usize, usize, usize) {
        let placement = self.placement.lock().expect("rozmieszczenie zatrute");
        let mut counts = (0usize, 0usize, 0usize);
        for slot in &placement.slot_of {
            match slot {
                Some(slot) => match self.slot_tier[*slot] {
                    ExpertTier::Vram => counts.0 += 1,
                    _ => counts.1 += 1,
                },
                None => counts.2 += 1,
            }
        }
        counts
    }

    /// Przenosi eksperta `promote` do VRAM, oddając jego dotychczasowe miejsce
    /// ekspertowi `demote`.
    ///
    /// Zamiast realokacji zamieniamy zawartość dwóch slotów: wszyscy eksperci
    /// jednej projekcji są równej wielkości, a pula wag i tak nie zwalnia
    /// pojedynczych bloków. Wywołujący MUSI mieć pewność, że urządzenie nie ma
    /// w locie pracy czytającej te wagi.
    pub fn promote_to_vram(
        &self,
        device: &dyn Device,
        promote: usize,
        demote: usize,
        scratch: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        let (host_slot, vram_slot) = {
            let placement = self.placement.lock().expect("rozmieszczenie zatrute");
            let host_slot = placement.slot_of[promote].ok_or_else(|| {
                ForgeError::Kernel(format!("ekspert {promote} nie jest rezydentny"))
            })?;
            let vram_slot = placement.slot_of[demote].ok_or_else(|| {
                ForgeError::Kernel(format!("ekspert {demote} nie jest rezydentny"))
            })?;
            (host_slot, vram_slot)
        };
        if self.slot_tier[vram_slot] != ExpertTier::Vram
            || self.slot_tier[host_slot] != ExpertTier::Host
        {
            return Err(ForgeError::Kernel(format!(
                "awans wymaga slotu VRAM i slotu hosta, dostano {:?}/{:?}",
                self.slot_tier[vram_slot], self.slot_tier[host_slot]
            )));
        }
        if scratch.len() < self.bytes_per_expert {
            return Err(ForgeError::Kernel(format!(
                "bufor przesiadkowy migracji ma {} B, potrzeba {}",
                scratch.len(),
                self.bytes_per_expert
            )));
        }
        let bytes = self.bytes_per_expert;
        let in_vram = slot_buffer(&self.slots[vram_slot])?;
        let in_host = slot_buffer(&self.slots[host_slot])?;
        device.copy(in_vram, 0, scratch, 0, bytes, stream)?;
        device.copy(in_host, 0, in_vram, 0, bytes, stream)?;
        device.copy(scratch, 0, in_host, 0, bytes, stream)?;
        stream.synchronize()?;

        {
            let mut placement = self.placement.lock().expect("rozmieszczenie zatrute");
            placement.owner_of[vram_slot] = promote;
            placement.owner_of[host_slot] = demote;
            placement.slot_of[promote] = Some(vram_slot);
            placement.slot_of[demote] = Some(host_slot);
        }
        self.rewrite_table(device)
    }

    /// Ściąga z dysku wszystkie żądane eksperty, które nie są rezydentne, jednym
    /// równoległym zgłoszeniem.
    ///
    /// Ofiarami są WYŁĄCZNIE sloty w pamięci hosta: sloty VRAM należą do
    /// najgorętszych ekspertów i rządzi nimi migracja, a czytanie z dysku wprost
    /// do VRAM i tak wymagałoby bufora pośredniego. Ofiary nie odkładamy nigdzie
    /// — wagi są tylko do odczytu, więc jej kopia na dysku nadal jest aktualna.
    ///
    /// Zwraca liczbę faktycznie ściągniętych ekspertów.
    pub fn fault_in(
        &self,
        device: &dyn Device,
        spill: &ExpertSpill,
        wanted: &[usize],
        popularity: &[f32],
    ) -> Result<usize> {
        let mut targets = Vec::new();
        {
            let mut placement = self.placement.lock().expect("rozmieszczenie zatrute");
            let mut protected = vec![false; self.n_experts];
            for &expert in wanted {
                if expert >= self.n_experts {
                    return Err(ForgeError::Kernel(format!(
                        "router wskazał eksperta {expert} spoza {}",
                        self.n_experts
                    )));
                }
                protected[expert] = true;
            }
            // Sloty hosta uporządkowane od najmniej popularnego właściciela —
            // stąd biorą się ofiary.
            let mut victims: Vec<usize> = (0..self.slots.len())
                .filter(|slot| self.slot_tier[*slot] == ExpertTier::Host)
                .collect();
            victims.sort_by(|a, b| {
                popularity[placement.owner_of[*a]].total_cmp(&popularity[placement.owner_of[*b]])
            });
            let mut next_victim = 0usize;
            for &expert in wanted {
                if placement.slot_of[expert].is_some() {
                    continue;
                }
                let region = placement.spilled[expert].ok_or_else(|| {
                    ForgeError::Kernel(format!(
                        "ekspert {expert} nie jest rezydentny i nie ma kopii na dysku"
                    ))
                })?;
                let slot = loop {
                    let slot = *victims.get(next_victim).ok_or_else(|| {
                        ForgeError::Kernel(
                            "za mało slotów w pamięci hosta na komplet wybranych ekspertów".into(),
                        )
                    })?;
                    next_victim += 1;
                    if !protected[placement.owner_of[slot]] {
                        break slot;
                    }
                };
                let evicted = placement.owner_of[slot];
                placement.slot_of[evicted] = None;
                placement.slot_of[expert] = Some(slot);
                placement.owner_of[slot] = expert;
                let host_ptr = slot_buffer(&self.slots[slot])?.host_ptr().ok_or_else(|| {
                    ForgeError::Kernel(
                        "slot warstwy hosta nie ma adresu widocznego dla hosta".into(),
                    )
                })?;
                targets.push(SpillTarget { region, host_ptr });
            }
        }
        if targets.is_empty() {
            return Ok(0);
        }
        let fetched = targets.len();
        spill.read_batch(&targets)?;
        self.rewrite_table(device)?;
        Ok(fetched)
    }
}

/// Bufor bajtów slotu; formaty wielobuforowe (compressed-tensors NVFP4) nie mają
/// jednego wskaźnika bazowego i nie mogą wejść w rezydencję.
fn slot_buffer(weight: &DevWeight) -> Result<&DevBuffer> {
    weight.buffer().ok_or_else(|| {
        ForgeError::Unsupported("rezydencja ekspertów wymaga wagi o jednym buforze bajtów".into())
    })
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
                // Ekspert z dysku wchodzi do pamięci przez stronicowanie na
                // żądanie, nie przez tę migrację — awans prosto z dysku do VRAM
                // wymagałby odczytu, którego runda w tle nie ma prawa robić.
                ExpertTier::Nvme => {}
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
                let (_, host, nvme) = stack.tier_counts();
                spilled |= host > 0 || nvme > 0;
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

