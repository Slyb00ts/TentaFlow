// ===== File: model/state_cache.rs — pula stanow DeltaNet i cache checkpointow =====
//
// Sekwencja hybrydowa niesie stan rekurencyjny, ktorego stronicowany KV nie
// opisuje: okno splotu i macierz stanu kazdej warstwy DeltaNet. Zyje on w
// slotach puli, po jednym na sekwencje. Prefiks wspoldzielony potrzebuje tego
// samego slotu w drugiej roli — jako CHECKPOINT pozycji, w ktorej konczy sie
// wspoldzielony prefiks — wiec obie role biora z JEDNEJ puli. Dzieki temu nie
// ma proporcji do strojenia: rosnaca wspolbieznosc odbiera sloty cache'owi.
use super::*;

/// Sloty, jakie pula odda pod checkpointy prefiksu — ponad te, które trzymają
/// żywe sekwencje.
///
/// To jest CAŁY budżet cache'u i celowo nie jest pokrętłem. Podział pamięci
/// między stan a strony nie jest tu proporcją do zgadnięcia: sloty żywych
/// sekwencji i sloty checkpointów pochodzą z jednej puli, więc rosnąca
/// współbieżność sama odbiera je cache'owi, a `--prefix-cache off` wyłącza go
/// w całości.
pub(crate) const HYBRID_STATE_CACHE_SLOTS: usize = 16;

/// Co ile tokenów prefill zatrzymuje się, żeby utrwalić stan.
///
/// Checkpoint na końcu promptu obsługuje POWTÓRZENIE tego promptu — kolejną turę
/// rozmowy. Nie obsługuje żądania, które dzieli prompt systemowy i kończy się
/// innym pytaniem, bo tam rozjazd wypada PRZED tym checkpointem. Dlatego stoją
/// też pośrednie, a ich cena to krótsze chunki prefillu.
pub(crate) const HYBRID_STATE_CHECKPOINT_STRIDE: usize = 512;

/// Ile tokenów przepuścić, zanim sekwencja stojąca na `at` dojdzie do najbliższej
/// pozycji wartej utrwalenia.
fn hybrid_checkpoint_step(at: usize, target: usize, remaining: usize) -> usize {
    let stride = HYBRID_STATE_CHECKPOINT_STRIDE;
    let next_stride = (at / stride + 1) * stride;
    let boundary = match target > at {
        true => next_stride.min(target),
        false => next_stride,
    };
    remaining.min(boundary - at).max(1)
}

/// One DeltaNet layer's resident recurrent state for the active sequence.
pub(crate) struct SsmState {
    /// Causal conv window `[conv_dim, d_conv-1]` f16 (oldest sample first).
    pub(crate) conv: DevBuffer,
    /// Recurrent state matrices `[n_v_heads, d_state, d_state]` f32.
    pub(crate) state: DevBuffer,
}

pub(crate) struct HybridStateSlot {
    pub(crate) layers: Vec<Option<SsmState>>,
    pub(crate) mtp: Option<MtpDraftState>,
    generation: u64,
    pub(crate) in_use: bool,
    ready: Event,
    ready_recorded: bool,
    initialized_generation: u64,
}

pub(crate) struct HybridStatePool {
    pub(crate) device: Arc<dyn Device>,
    layer_kinds: Vec<LayerKind>,
    layout: DeltaStateLayout,
    conv_bytes: usize,
    state_bytes: usize,
    zero_conv: DevBuffer,
    zero_state: DevBuffer,
    pub(crate) slots: Vec<HybridStateSlot>,
    pub(crate) free: Vec<usize>,
    active: Option<HybridStateLease>,
    pub(crate) poisoned: Option<String>,
    pub(crate) mtp_kv: Option<KvCache>,
    mtp_shape: Option<(usize, usize)>,
    pub(crate) quarantined_mtp_states: Vec<MtpDraftState>,
    pub(crate) quarantined_mtp_kv: Vec<KvCache>,
    /// Sloty, jakie pula odda pod checkpointy prefiksu. Zero = cache wyłączony.
    cache_limit: usize,
    /// Sloty trzymane teraz w tej roli — ani wolne, ani wydzierżawione.
    cached: usize,
}

impl HybridStatePool {
    pub(crate) fn new(
        device: Arc<dyn Device>,
        layer_kinds: Vec<LayerKind>,
        layout: DeltaStateLayout,
        conv_bytes: usize,
        state_bytes: usize,
        mtp_config: Option<(KvConfig, usize, usize)>,
        cache_limit: usize,
    ) -> Result<Self> {
        let zero_conv = device.alloc(conv_bytes, MemKind::PinnedHost, Pool::Activations)?;
        let zero_state = device.alloc(state_bytes, MemKind::PinnedHost, Pool::Activations)?;
        unsafe {
            std::ptr::write_bytes(
                zero_conv.host_ptr().expect("pinned host mapping"),
                0,
                conv_bytes,
            );
            std::ptr::write_bytes(
                zero_state.host_ptr().expect("pinned host mapping"),
                0,
                state_bytes,
            );
        }
        let (mtp_kv, mtp_shape) = match mtp_config {
            Some((config, hidden_size, vocab_size)) => (
                Some(KvCache::new(device.as_ref(), config)?),
                Some((hidden_size, vocab_size)),
            ),
            None => (None, None),
        };
        let mut pool = Self {
            device,
            layer_kinds,
            layout,
            conv_bytes,
            state_bytes,
            zero_conv,
            zero_state,
            slots: Vec::new(),
            free: Vec::new(),
            active: None,
            poisoned: None,
            mtp_kv,
            mtp_shape,
            quarantined_mtp_states: Vec::new(),
            quarantined_mtp_kv: Vec::new(),
            cache_limit,
            cached: 0,
        };
        pool.allocate_slot()?;
        Ok(pool)
    }

    fn build_slot(&self) -> Result<HybridStateSlot> {
        let mtp = match (&self.mtp_kv, self.mtp_shape) {
            (Some(kv), Some((hidden_size, vocab_size))) => Some(MtpDraftState::new(
                self.device.clone(),
                kv,
                hidden_size,
                vocab_size,
            )?),
            (None, None) => None,
            _ => {
                return Err(ForgeError::Scheduler(
                    "niespójna konfiguracja puli MTP".into(),
                ))
            }
        };
        let ready = self.device.create_event()?;
        let mut layers = Vec::with_capacity(self.layer_kinds.len());
        for kind in &self.layer_kinds {
            layers.push(match kind {
                LayerKind::DeltaNet => Some(SsmState {
                    conv: self
                        .device
                        .alloc(self.conv_bytes, MemKind::Device, Pool::Weights)?,
                    state: self
                        .device
                        .alloc(self.state_bytes, MemKind::Device, Pool::Weights)?,
                }),
                LayerKind::Attention => None,
            });
        }
        Ok(HybridStateSlot {
            layers,
            mtp,
            generation: 0,
            in_use: false,
            ready,
            ready_recorded: false,
            initialized_generation: 0,
        })
    }

    fn allocate_slot(&mut self) -> Result<usize> {
        let state = self.build_slot()?;
        let slot = self.slots.len();
        self.slots.push(state);
        self.free.push(slot);
        Ok(slot)
    }

    pub(crate) fn ensure_capacity(&mut self, slots: usize) -> Result<()> {
        self.ensure_healthy()?;
        // Sloty trzymane pod checkpointy nie liczą się do pojemności żywych
        // sekwencji — preflight ma zapewnić miejsce na `slots` lease'ów, a nie
        // na `slots` slotów w ogóle.
        let additional = slots.saturating_sub(self.slots.len() - self.cached);
        if additional == 0 {
            return Ok(());
        }
        let weights_per_slot = self.slot_weight_bytes()?;
        let reserve = |bytes: usize| {
            bytes
                .max(1)
                .checked_next_multiple_of(DEVICE_ALLOC_ALIGN)
                .ok_or_else(|| ForgeError::Scheduler("przepełnienie wyrównania alokacji".into()))
        };
        let activations_per_slot = match (self.mtp_shape, self.mtp_kv.as_ref()) {
            (Some((hidden, vocab)), Some(kv)) => {
                let hidden_bytes = hidden.checked_mul(2).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie rozmiaru hidden MTP".into())
                })?;
                let step_hidden = hidden_bytes.checked_mul(4).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie checkpointów hidden MTP".into())
                })?;
                let logits = vocab
                    .checked_mul(4)
                    .ok_or_else(|| ForgeError::Scheduler("przepełnienie logitów MTP".into()))?;
                let page_table = kv.cfg.max_pages_per_seq.checked_mul(4).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie tabeli stron MTP".into())
                })?;
                [
                    hidden_bytes,
                    hidden_bytes,
                    hidden_bytes,
                    logits,
                    page_table,
                    4,
                    4,
                    20,
                    hidden_bytes,
                    step_hidden,
                ]
                .into_iter()
                .try_fold(0usize, |total, bytes| {
                    total.checked_add(reserve(bytes)?).ok_or_else(|| {
                        ForgeError::Scheduler("przepełnienie rozmiaru slotu MTP".into())
                    })
                })?
            }
            (None, None) => 0,
            _ => {
                return Err(ForgeError::Scheduler(
                    "niespójna konfiguracja puli MTP".into(),
                ))
            }
        };
        let required_weights = weights_per_slot
            .checked_mul(additional)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie preflightu puli SSM".into()))?;
        let required_activations = activations_per_slot
            .checked_mul(additional)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie preflightu puli MTP".into()))?;
        let available_weights = self.device.pool_available(Pool::Weights);
        let available_activations = self.device.pool_available(Pool::Activations);
        if available_weights.is_some_and(|available| required_weights > available)
            || available_activations.is_some_and(|available| required_activations > available)
        {
            return Err(ForgeError::Scheduler(format!(
                "preflight {slots} slotów hybrydowych wymaga {required_weights} B puli weights i {required_activations} B puli activations dla {additional} nowych slotów; dostępne odpowiednio {} B i {} B",
                available_weights.map_or_else(|| "nieznane".into(), |bytes| bytes.to_string()),
                available_activations
                    .map_or_else(|| "nieznane".into(), |bytes| bytes.to_string()),
            )));
        }
        let mut allocated = Vec::with_capacity(additional);
        for _ in 0..additional {
            allocated.push(self.build_slot().map_err(|error| {
                ForgeError::Scheduler(format!(
                    "preflight {slots} slotów hybrydowych nie zaalokował {additional} nowych slotów (weights {required_weights} B, activations {required_activations} B): {error}"
                ))
            })?);
        }
        let first = self.slots.len();
        self.slots.extend(allocated);
        self.free.extend(first..first + additional);
        Ok(())
    }

    pub(crate) fn acquire(&mut self) -> Result<HybridStateLease> {
        self.ensure_healthy()?;
        if self.free.is_empty() {
            self.allocate_slot()?;
        }
        let slot = self.free.pop().expect("wolny slot został przygotowany");
        let state = &mut self.slots[slot];
        state.generation = state.generation.checked_add(1).ok_or_else(|| {
            ForgeError::Scheduler("licznik generacji stanu hybrydowego został wyczerpany".into())
        })?;
        state.in_use = true;
        Ok(HybridStateLease {
            slot,
            generation: state.generation,
        })
    }

    pub(crate) fn validate(&self, lease: HybridStateLease) -> Result<()> {
        let Some(slot) = self.slots.get(lease.slot) else {
            return Err(ForgeError::Scheduler(
                "nieprawidłowy slot stanu hybrydowego".into(),
            ));
        };
        if !slot.in_use || slot.generation != lease.generation {
            return Err(ForgeError::Scheduler(
                "nieaktualny lease stanu hybrydowego".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn ensure_healthy(&self) -> Result<()> {
        match &self.poisoned {
            Some(reason) => Err(ForgeError::Device(format!(
                "pula stanów hybrydowych jest zatruta: {reason}"
            ))),
            None => Ok(()),
        }
    }

    pub(crate) fn poison(&mut self, reason: String) -> ForgeError {
        self.poisoned = Some(reason.clone());
        ForgeError::Device(format!("pula stanów hybrydowych została zatruta: {reason}"))
    }

    pub(crate) fn quarantine_mtp(
        &mut self,
        reason: String,
        states: impl IntoIterator<Item = MtpDraftState>,
        kv: KvCache,
    ) -> ForgeError {
        self.quarantined_mtp_states.extend(states);
        self.quarantined_mtp_kv.push(kv);
        self.poison(reason)
    }

    pub(crate) fn activate(&mut self, lease: HybridStateLease, stream: &Stream) -> Result<()> {
        self.ensure_healthy()?;
        self.validate(lease)?;
        if self.active == Some(lease) {
            return Ok(());
        }
        // Aktywne lease współdzielą jeden stream, więc ich praca jest już
        // uporządkowana; event jest potrzebny dopiero między generacjami slotu.
        let slot = &mut self.slots[lease.slot];
        if slot.ready_recorded {
            self.device.wait_event(stream, &slot.ready)?;
            slot.ready_recorded = false;
        }
        if slot.initialized_generation != lease.generation {
            for state in slot.layers.iter().flatten() {
                self.device
                    .copy(&self.zero_conv, 0, &state.conv, 0, self.conv_bytes, stream)?;
                self.device.copy(
                    &self.zero_state,
                    0,
                    &state.state,
                    0,
                    self.state_bytes,
                    stream,
                )?;
            }
            // Stan draftu MTP należy do TEGO SAMEGO slotu, więc czyści się z
            // nim razem. Wcześniej kasował go prefill przy pozycji zerowej —
            // sekwencja zaczynająca od pożyczonego prefiksu nigdy tam nie
            // trafia i odziedziczyłaby cudze strony draftu.
            if let (Some(state), Some(kv)) = (slot.mtp.as_mut(), self.mtp_kv.as_mut()) {
                state.reset(kv, stream)?;
            }
            slot.initialized_generation = lease.generation;
        }
        self.active = Some(lease);
        Ok(())
    }

    pub(crate) fn release(&mut self, lease: HybridStateLease, stream: &Stream) -> Result<()> {
        self.ensure_healthy()?;
        self.validate(lease)?;
        let record_result = self
            .device
            .record_event(&self.slots[lease.slot].ready, stream);
        self.finish_release(lease, record_result, || stream.synchronize())
    }

    pub(crate) fn finish_release(
        &mut self,
        lease: HybridStateLease,
        record_result: Result<()>,
        synchronize: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let event_recorded = match record_result {
            Ok(()) => true,
            Err(record_error) => match synchronize() {
                Ok(()) => {
                    if let Err(zero_error) = self.zero_slot_synchronously(lease.slot) {
                        let reason = format!(
                            "record eventu nie powiódł się ({record_error}); po synchronizacji nie udało się wyzerować slotu ({zero_error})"
                        );
                        self.poisoned = Some(reason.clone());
                        return Err(ForgeError::Device(reason));
                    }
                    tracing::warn!(
                        "record eventu zwolnienia stanu hybrydowego nie powiódł się; stream został zsynchronizowany: {record_error}"
                    );
                    false
                }
                Err(sync_error) => {
                    let reason = format!(
                        "record eventu nie powiódł się ({record_error}); synchronizacja streamu także nie powiodła się ({sync_error})"
                    );
                    self.poisoned = Some(reason.clone());
                    return Err(ForgeError::Device(reason));
                }
            },
        };
        let slot = &mut self.slots[lease.slot];
        slot.ready_recorded = event_recorded;
        if self.active == Some(lease) {
            self.active = None;
        }
        slot.in_use = false;
        if let (Some(kv), Some(mtp)) = (&mut self.mtp_kv, &mut slot.mtp) {
            mtp.release(kv);
        }
        self.free.push(lease.slot);
        Ok(())
    }

    fn zero_slot_synchronously(&self, slot_index: usize) -> Result<()> {
        let zero_conv = unsafe {
            std::slice::from_raw_parts(
                self.zero_conv.host_ptr().expect("pinned host mapping"),
                self.conv_bytes,
            )
        };
        let zero_state = unsafe {
            std::slice::from_raw_parts(
                self.zero_state.host_ptr().expect("pinned host mapping"),
                self.state_bytes,
            )
        };
        for state in self.slots[slot_index].layers.iter().flatten() {
            self.device.write(zero_conv, &state.conv, 0)?;
            self.device.write(zero_state, &state.state, 0)?;
        }
        Ok(())
    }

    pub(crate) fn reset(&mut self, lease: HybridStateLease, stream: &Stream) -> Result<()> {
        self.activate(lease, stream)?;
        let slot = &mut self.slots[lease.slot];
        for state in slot.layers.iter().flatten() {
            self.device
                .copy(&self.zero_conv, 0, &state.conv, 0, self.conv_bytes, stream)?;
            self.device.copy(
                &self.zero_state,
                0,
                &state.state,
                0,
                self.state_bytes,
                stream,
            )?;
        }
        slot.initialized_generation = lease.generation;
        Ok(())
    }

    pub(crate) fn active_layers(&self) -> &[Option<SsmState>] {
        let active = self.active.expect("stan hybrydowy został aktywowany");
        &self.slots[active.slot].layers
    }

    pub(crate) fn layout(&self) -> DeltaStateLayout {
        self.layout
    }

    pub(crate) fn state_buffers(
        &self,
        lease: HybridStateLease,
        layer: usize,
    ) -> Result<Option<(DevBuffer, DevBuffer)>> {
        self.validate(lease)?;
        Ok(self.slots[lease.slot]
            .layers
            .get(layer)
            .ok_or_else(|| ForgeError::Scheduler("warstwa stanu hybrydowego poza zakresem".into()))?
            .as_ref()
            .map(|state| (state.conv.clone(), state.state.clone())))
    }

    pub(crate) fn has_mtp(&self) -> bool {
        self.mtp_kv.is_some() && self.mtp_shape.is_some()
    }

    pub(crate) fn take_mtp(&mut self, lease: HybridStateLease) -> Result<(MtpDraftState, KvCache)> {
        self.ensure_healthy()?;
        self.validate(lease)?;
        let kv = self.mtp_kv.take().ok_or_else(|| {
            ForgeError::Unsupported("współdzielony cache MTP nie został zaalokowany".into())
        })?;
        let state = match self.slots[lease.slot].mtp.take() {
            Some(state) => state,
            None => {
                self.mtp_kv = Some(kv);
                return Err(ForgeError::Scheduler(
                    "stan MTP aktywnej sekwencji jest już używany".into(),
                ));
            }
        };
        Ok((state, kv))
    }

    pub(crate) fn take_mtp_pair(
        &mut self,
        leases: [HybridStateLease; 2],
    ) -> Result<([MtpDraftState; 2], KvCache)> {
        self.ensure_healthy()?;
        if leases[0].slot == leases[1].slot {
            return Err(ForgeError::Scheduler(
                "para MTP wymaga dwóch różnych slotów".into(),
            ));
        }
        self.validate(leases[0])?;
        self.validate(leases[1])?;
        let kv = self.mtp_kv.take().ok_or_else(|| {
            ForgeError::Unsupported("współdzielony cache MTP nie został zaalokowany".into())
        })?;
        let first = match self.slots[leases[0].slot].mtp.take() {
            Some(state) => state,
            None => {
                self.mtp_kv = Some(kv);
                return Err(ForgeError::Scheduler(
                    "pierwszy stan pary MTP jest już używany".into(),
                ));
            }
        };
        let second = match self.slots[leases[1].slot].mtp.take() {
            Some(state) => state,
            None => {
                self.slots[leases[0].slot].mtp = Some(first);
                self.mtp_kv = Some(kv);
                return Err(ForgeError::Scheduler(
                    "drugi stan pary MTP jest już używany".into(),
                ));
            }
        };
        Ok(([first, second], kv))
    }

    pub(crate) fn restore_mtp(
        &mut self,
        lease: HybridStateLease,
        state: MtpDraftState,
        kv: KvCache,
    ) -> Result<()> {
        let preflight = self
            .ensure_healthy()
            .and_then(|_| self.validate(lease))
            .and_then(|_| {
                if self.mtp_kv.is_some() || self.slots[lease.slot].mtp.is_some() {
                    Err(ForgeError::Scheduler(
                        "próba podwójnego przywrócenia stanu MTP".into(),
                    ))
                } else {
                    Ok(())
                }
            });
        if let Err(error) = preflight {
            let reason = format!("przywrócenie stanu MTP nie powiodło się: {error}");
            return Err(self.quarantine_mtp(reason, [state], kv));
        }
        self.mtp_kv = Some(kv);
        self.slots[lease.slot].mtp = Some(state);
        Ok(())
    }

    pub(crate) fn restore_mtp_pair(
        &mut self,
        leases: [HybridStateLease; 2],
        states: [MtpDraftState; 2],
        kv: KvCache,
    ) -> Result<()> {
        let preflight = self
            .ensure_healthy()
            .and_then(|_| {
                if leases[0].slot == leases[1].slot {
                    Err(ForgeError::Scheduler(
                        "para MTP wymaga dwóch różnych slotów".into(),
                    ))
                } else {
                    Ok(())
                }
            })
            .and_then(|_| self.validate(leases[0]))
            .and_then(|_| self.validate(leases[1]))
            .and_then(|_| {
                if self.mtp_kv.is_some()
                    || self.slots[leases[0].slot].mtp.is_some()
                    || self.slots[leases[1].slot].mtp.is_some()
                {
                    Err(ForgeError::Scheduler(
                        "próba podwójnego przywrócenia pary stanów MTP".into(),
                    ))
                } else {
                    Ok(())
                }
            });
        if let Err(error) = preflight {
            let reason = format!("przywrócenie pary stanów MTP nie powiodło się: {error}");
            return Err(self.quarantine_mtp(reason, states, kv));
        }
        let [first, second] = states;
        self.mtp_kv = Some(kv);
        self.slots[leases[0].slot].mtp = Some(first);
        self.slots[leases[1].slot].mtp = Some(second);
        Ok(())
    }

    pub(crate) fn mtp_host_embedding_gathers(&self) -> u64 {
        self.slots
            .iter()
            .filter_map(|slot| slot.mtp.as_ref())
            .map(MtpDraftState::host_embedding_gathers)
            .sum()
    }
}

impl HybridStatePool {
    /// Bajty puli `Weights`, jakie zajmuje jeden slot stanu.
    fn slot_weight_bytes(&self) -> Result<usize> {
        let delta_layers = self
            .layer_kinds
            .iter()
            .filter(|kind| matches!(kind, LayerKind::DeltaNet))
            .count();
        let reserve = |bytes: usize| {
            bytes
                .max(1)
                .checked_next_multiple_of(DEVICE_ALLOC_ALIGN)
                .ok_or_else(|| ForgeError::Scheduler("przepełnienie wyrównania alokacji".into()))
        };
        reserve(self.conv_bytes)?
            .checked_add(reserve(self.state_bytes)?)
            .and_then(|bytes| bytes.checked_mul(delta_layers))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie rozmiaru slotu SSM".into()))
    }

    /// Zajmuje slot pod checkpoint prefiksu.
    ///
    /// `Ok(None)` znaczy „nie teraz": limit wyczerpany albo pula `Weights` nie
    /// ma miejsca na kolejny slot. Cache jest optymalizacją, więc brak slotu
    /// zostawia sekwencję na zwykłej ścieżce zamiast przewracać żądanie.
    pub(crate) fn take_cache_slot(&mut self) -> Result<Option<usize>> {
        self.ensure_healthy()?;
        if self.cached >= self.cache_limit {
            return Ok(None);
        }
        let slot = match self.free.pop() {
            Some(slot) => slot,
            None => {
                let need = self.slot_weight_bytes()?;
                if self
                    .device
                    .pool_available(Pool::Weights)
                    .is_some_and(|available| need > available)
                {
                    return Ok(None);
                }
                let slot = self.allocate_slot()?;
                self.free
                    .pop()
                    .expect("świeży slot trafia na listę wolnych");
                slot
            }
        };
        self.slots[slot].in_use = true;
        self.cached += 1;
        Ok(Some(slot))
    }

    /// Oddaje slot checkpointu z powrotem na listę wolnych.
    pub(crate) fn put_cache_slot(&mut self, slot: usize) {
        debug_assert!(self.cached > 0, "zwrot slotu bez pobrania");
        self.cached = self.cached.saturating_sub(1);
        self.slots[slot].in_use = false;
        self.free.push(slot);
    }

    /// Kopiuje stan wydzierżawionego slotu do slotu checkpointu (D2D).
    pub(crate) fn snapshot(
        &mut self,
        lease: HybridStateLease,
        slot: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.validate(lease)?;
        self.await_slot(slot, stream)?;
        self.copy_layers(lease.slot, slot, stream)
    }

    /// Kopiuje checkpoint w stan wydzierżawionego slotu (D2D).
    pub(crate) fn restore(
        &mut self,
        lease: HybridStateLease,
        slot: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.validate(lease)?;
        self.await_slot(slot, stream)?;
        self.copy_layers(slot, lease.slot, stream)?;
        self.slots[lease.slot].initialized_generation = lease.generation;
        Ok(())
    }

    /// Czeka na pracę zakolejkowaną przez POPRZEDNIEGO właściciela slotu.
    /// Slot wraca na listę wolnych z zapisanym eventem, a checkpoint pisze do
    /// niego z innej generacji niż ta, która go zwolniła.
    fn await_slot(&mut self, slot: usize, stream: &Stream) -> Result<()> {
        let held = self
            .slots
            .get_mut(slot)
            .ok_or_else(|| ForgeError::Scheduler("slot checkpointu poza zakresem".into()))?;
        if held.ready_recorded {
            self.device.wait_event(stream, &held.ready)?;
            self.slots[slot].ready_recorded = false;
        }
        Ok(())
    }

    fn copy_layers(&self, from: usize, to: usize, stream: &Stream) -> Result<()> {
        if from == to {
            return Err(ForgeError::Scheduler(
                "kopia stanu DeltaNet w ten sam slot".into(),
            ));
        }
        for (src, dst) in self.slots[from]
            .layers
            .iter()
            .zip(self.slots[to].layers.iter())
        {
            let (Some(src), Some(dst)) = (src, dst) else {
                continue;
            };
            self.device
                .copy(&src.conv, 0, &dst.conv, 0, self.conv_bytes, stream)?;
            self.device
                .copy(&src.state, 0, &dst.state, 0, self.state_bytes, stream)?;
        }
        Ok(())
    }
}

impl Model {
    /// Prefill hybrydy, który po drodze utrwala stan rekurencyjny.
    ///
    /// Prefiks wspoldzielony jest całostronicowy, więc checkpoint musi stanąć
    /// dokładnie na granicy strony — inaczej strony i stan opisywałyby dwie
    /// różne pozycje. Chunk, który taką granicę przekracza, jest w niej dzielony.
    pub(crate) fn prefill_hybrid_checkpointed(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
    ) -> Result<Vec<f32>> {
        if self.prefix_cache.is_none() {
            return self.prefill_hybrid(seq, tokens);
        }
        let mut logits = Vec::new();
        let mut done = 0;
        while done < tokens.len() {
            let take = hybrid_checkpoint_step(seq.len, seq.state_target, tokens.len() - done);
            logits = self.prefill_hybrid_recorded(seq, &tokens[done..done + take])?;
            done += take;
            self.snapshot_hybrid_state(seq)?;
        }
        Ok(logits)
    }

    /// Prefill jednego chunka z zapisem tokenów, na których stoi darowizna.
    ///
    /// Zapis idzie PO przebiegu, nie przed: rollback layer-major przywraca
    /// `tokens`/`prefilled_len` do wartości sprzed chunka, a te wartości bierze
    /// przy wejściu — dopisanie wcześniej zapisałoby tokeny, których nieudany
    /// chunk nigdy nie policzył.
    fn prefill_hybrid_recorded(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        let logits = self.prefill_hybrid(seq, tokens)?;
        if seq.tokens.len() == seq.prefilled_len {
            seq.prefilled_len += tokens.len();
        }
        seq.tokens.extend_from_slice(tokens);
        Ok(logits)
    }

    /// Utrwala stan sekwencji, gdy stoi ona na granicy strony.
    ///
    /// Granularność jest ta, którą i tak wyznacza scheduler: kwant prefillu jest
    /// wielokrotnością strony, więc każda granica chunka poza ostatnią wypada na
    /// stronie za darmo. Jedyny wymuszony podział to ten na `state_target`, a
    /// pośrednie checkpointy są tym, co pozwala pożyczyć wspólny prompt
    /// systemowy pytaniu, które kończy się inaczej.
    fn snapshot_hybrid_state(&mut self, seq: &mut SeqKv) -> Result<()> {
        let at = seq.len;
        if at == 0 || at > seq.state_target || !at.is_multiple_of(self.kv.cfg.page_size) {
            return Ok(());
        }
        if seq.state_checkpoints.iter().any(|&(pos, _)| pos == at) {
            return Ok(());
        }
        let Some(slot) = self.claim_state_slot()? else {
            return Ok(());
        };
        if let Err(error) = self.write_state_slot(seq, slot) {
            self.release_state_slot(slot);
            return Err(error);
        }
        seq.state_checkpoints.push((at, slot));
        Ok(())
    }

    /// Przesuwa TOCZĄCY SIĘ checkpoint dekodowania na bieżącą granicę strony.
    ///
    /// Wygenerowana odpowiedź jest prefiksem następnej tury, ale strony hybrydy
    /// są osiągalne wyłącznie z checkpointu — bez tego cała odpowiedź byłaby
    /// dla drzewa niewidoczna. Slot jest JEDEN na sekwencję i nadpisywany co
    /// stronę, więc długi dekod nie zjada puli; kopia D2D co 32 tokeny to
    /// ułamek kroku dekodowania.
    pub(crate) fn roll_hybrid_checkpoint(&mut self, seq: &mut SeqKv) -> Result<()> {
        // Sloty unieważnione przez rollback wracają do puli tutaj: to pierwszy
        // punkt po każdym cofnięciu, w którym pula jest osiągalna.
        if !seq.state_orphans.is_empty() {
            self.return_state_slots(std::mem::take(&mut seq.state_orphans));
        }
        let at = seq.len;
        if self.prefix_cache.is_none()
            || at <= seq.state_target
            || !at.is_multiple_of(self.kv.cfg.page_size)
        {
            return Ok(());
        }
        // Drzewo kluczuje stronę jej tokenami, a weryfikacja spekulacyjna
        // wysuwa `len` przed listę tokenów: dopóki draft nie jest rozstrzygnięty,
        // te pozycje nie mają właściciela, więc checkpoint stamtąd byłby nie do
        // zaadresowania.
        if seq.tokens.len() < at {
            return Ok(());
        }
        if seq.state_rolling.is_some_and(|(pos, _)| pos == at) {
            return Ok(());
        }
        // Slot wychodzi ze śledzenia na czas zapisu, żeby nieudany zrzut wracał
        // do puli dokładnie raz — i przy pierwszym checkpoincie, i przy każdym
        // kolejnym nadpisaniu tego samego slotu.
        let slot = match seq.state_rolling.take() {
            Some((_, slot)) => slot,
            None => match self.claim_state_slot()? {
                Some(slot) => slot,
                None => return Ok(()),
            },
        };
        if let Err(error) = self.write_state_slot(seq, slot) {
            self.release_state_slot(slot);
            return Err(error);
        }
        seq.state_rolling = Some((at, slot));
        Ok(())
    }

    /// Bierze slot pod checkpoint, w razie potrzeby odbierając go drzewu.
    fn claim_state_slot(&mut self) -> Result<Option<usize>> {
        let mut reclaimed = None;
        let pool = self
            .hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów");
        let mut slot = pool.take_cache_slot()?;
        if slot.is_none() {
            // Pula nie ma czym płacić, więc płaci najzimniejszy checkpoint
            // drzewa. To jest cała „elastyczność" tego podziału: sloty żywych
            // sekwencji i sloty cache'u pochodzą z jednego zbioru.
            reclaimed = self
                .prefix_cache
                .as_mut()
                .and_then(|pc| pc.evict_states(1).pop());
        }
        if let Some(free) = reclaimed {
            let pool = self
                .hybrid_states
                .as_mut()
                .expect("model hybrydowy ma pulę stanów");
            pool.put_cache_slot(free);
            slot = pool.take_cache_slot()?;
        }
        Ok(slot)
    }

    /// Zrzuca stan sekwencji do wskazanego slotu.
    fn write_state_slot(&mut self, seq: &SeqKv, slot: usize) -> Result<()> {
        let Some(lease) = seq.hybrid_state else {
            return Ok(());
        };
        let stream = self.stream.clone();
        self.hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .snapshot(lease, slot, &stream)
    }

    /// Wgrywa checkpoint pożyczony przy przyjęciu w stan tej sekwencji.
    pub(crate) fn restore_hybrid_checkpoint(&mut self, seq: &mut SeqKv) -> Result<()> {
        let (Some(lease), Some(slot)) = (seq.hybrid_state, seq.state_restore) else {
            return Ok(());
        };
        let stream = self.stream.clone();
        self.hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .restore(lease, slot, &stream)?;
        seq.state_restore = None;
        Ok(())
    }

    /// Oddaje puli sloty, które drzewo właśnie zwolniło.
    pub(crate) fn return_state_slots(&mut self, slots: Vec<usize>) {
        for slot in slots {
            self.release_state_slot(slot);
        }
    }

    /// Oddaje puli jeden slot checkpointu.
    fn release_state_slot(&mut self, slot: usize) {
        self.hybrid_states
            .as_mut()
            .expect("checkpointy istnieją tylko dla modelu hybrydowego")
            .put_cache_slot(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_hal::cpu::CpuDevice;

    #[test]
    fn krok_prefillu_zatrzymuje_sie_na_kolejnym_checkpoincie() {
        let stride = HYBRID_STATE_CHECKPOINT_STRIDE;
        // Od zera do pierwszego stride'u, choćby chunk był dłuższy.
        assert_eq!(hybrid_checkpoint_step(0, 4096, 4096), stride);
        // Cel promptu wygrywa, gdy wypada wcześniej niż kolejny stride.
        assert_eq!(hybrid_checkpoint_step(stride, stride + 96, 4096), 96);
        // Za celem nie ma już czego dzielić — zostaje ogon strony.
        assert_eq!(hybrid_checkpoint_step(stride + 96, stride + 96, 17), 17);
        // Krok nigdy nie jest zerowy, nawet stojąc dokładnie na celu.
        assert_eq!(hybrid_checkpoint_step(stride, stride, 3), 3);
    }

    #[test]
    fn pula_stanow_izoluje_przeplatane_sekwencje_i_zeruje_reuzyty_slot() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mut pool = HybridStatePool::new(
            device.clone(),
            vec![LayerKind::DeltaNet, LayerKind::Attention],
            DeltaStateLayout::KeyValue,
            8,
            16,
            None,
            0,
        )
        .expect("pula powinna powstać");
        let first = pool.acquire().expect("pierwszy lease powinien powstać");
        let second = pool.acquire().expect("drugi lease powinien powstać");
        assert_ne!(first.slot, second.slot);

        pool.activate(first, &stream)
            .expect("pierwszy stan powinien się aktywować");
        let first_state = pool.active_layers()[0]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        device
            .write(&[7; 16], &first_state.state, 0)
            .expect("zapis pierwszego stanu powinien się udać");

        pool.activate(second, &stream)
            .expect("drugi stan powinien się aktywować");
        let second_state = pool.active_layers()[0]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        let mut bytes = [0xff; 16];
        device
            .read(&second_state.state, 0, &mut bytes)
            .expect("odczyt drugiego stanu powinien się udać");
        assert_eq!(bytes, [0; 16]);
        device
            .write(&[9; 16], &second_state.state, 0)
            .expect("zapis drugiego stanu powinien się udać");

        pool.activate(first, &stream)
            .expect("pierwszy stan powinien wrócić");
        let first_state = pool.active_layers()[0]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        device
            .read(&first_state.state, 0, &mut bytes)
            .expect("odczyt pierwszego stanu powinien się udać");
        assert_eq!(bytes, [7; 16]);

        pool.release(first, &stream)
            .expect("pierwszy lease powinien się zwolnić");
        let reused = pool.acquire().expect("slot powinien wrócić do puli");
        assert_eq!(reused.slot, first.slot);
        assert!(reused.generation > first.generation);
        pool.activate(reused, &stream)
            .expect("ponownie użyty slot powinien się aktywować");
        let reused_state = pool.active_layers()[0]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        device
            .read(&reused_state.state, 0, &mut bytes)
            .expect("odczyt ponownie użytego stanu powinien się udać");
        assert_eq!(bytes, [0; 16]);
        assert!(pool.release(first, &stream).is_err());
    }

    #[test]
    fn wspoldzielony_cache_mtp_obsluguje_cancel_release_i_reuse() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mtp_config = KvConfig {
            n_layers: 1,
            n_kv_heads: 1,
            head_dim: 8,
            page_size: 2,
            n_pages: 8,
            max_pages_per_seq: 8,
            quant: KvQuant::F16,
        };
        let mut pool = HybridStatePool::new(
            device,
            vec![LayerKind::DeltaNet],
            DeltaStateLayout::KeyValue,
            8,
            16,
            Some((mtp_config, 4, 8)),
            0,
        )
        .expect("pula z MTP powinna powstać");
        let first = pool.acquire().expect("pierwszy lease powinien powstać");
        let second = pool.acquire().expect("drugi lease powinien powstać");

        pool.activate(first, &stream)
            .expect("pierwszy lease powinien się aktywować");
        let (mut first_state, mut kv) = pool
            .take_mtp(first)
            .expect("stan MTP powinien być dostępny");
        first_state
            .grow(&mut kv)
            .expect("pierwsza strona powinna powstać");
        first_state
            .grow(&mut kv)
            .expect("pierwsza strona powinna się wypełnić");
        let first_pages = first_state.seq.pages.clone();
        pool.restore_mtp(first, first_state, kv)
            .expect("pierwszy stan powinien wrócić do slotu");

        pool.activate(second, &stream)
            .expect("drugi lease powinien się aktywować");
        let (mut second_state, mut kv) = pool
            .take_mtp(second)
            .expect("stan MTP powinien być dostępny");
        second_state
            .grow(&mut kv)
            .expect("druga strona powinna powstać");
        assert!(!first_pages.contains(&second_state.seq.pages[0]));
        second_state
            .checkpoint(&stream)
            .expect("checkpoint powinien powstać");
        second_state
            .grow(&mut kv)
            .expect("draft powinien zająć kolejną pozycję");
        second_state
            .rollback(&mut kv, &stream)
            .expect("cancel powinien odtworzyć długość bazową");
        assert_eq!(second_state.seq.len, 1);
        pool.restore_mtp(second, second_state, kv)
            .expect("drugi stan powinien wrócić do slotu");

        pool.release(first, &stream)
            .expect("pierwszy lease powinien się zwolnić");
        let reused = pool.acquire().expect("zwolniony slot powinien wrócić");
        assert_eq!(reused.slot, first.slot);
        assert!(reused.generation > first.generation);
        pool.activate(reused, &stream)
            .expect("ponownie użyty slot powinien się aktywować");
        let (reused_state, kv) = pool
            .take_mtp(reused)
            .expect("stan MTP powinien być dostępny");
        assert_eq!(reused_state.seq.len, 0);
        assert!(reused_state.seq.pages.is_empty());
        assert_eq!(kv.free_page_count(), 7);
        pool.restore_mtp(reused, reused_state, kv)
            .expect("stan po reuse powinien wrócić do slotu");

        pool.release(second, &stream)
            .expect("drugi lease powinien się zwolnić");
        pool.release(reused, &stream)
            .expect("ponownie użyty lease powinien się zwolnić");
        assert_eq!(
            pool.mtp_kv
                .as_ref()
                .expect("cache MTP powinien istnieć")
                .free_page_count(),
            8
        );
    }

    fn testowa_pula_mtp(device: Arc<dyn Device>) -> HybridStatePool {
        HybridStatePool::new(
            device,
            vec![LayerKind::DeltaNet],
            DeltaStateLayout::KeyValue,
            8,
            16,
            Some((
                KvConfig {
                    n_layers: 1,
                    n_kv_heads: 1,
                    head_dim: 8,
                    page_size: 2,
                    n_pages: 8,
                    max_pages_per_seq: 8,
                    quant: KvQuant::F16,
                },
                4,
                8,
            )),
            0,
        )
        .expect("testowa pula MTP powinna powstać")
    }

    #[test]
    fn prewalidacja_commitu_pary_mtp_nie_mutuje_zadnej_lane() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mut pool = testowa_pula_mtp(device);
        let leases = [
            pool.acquire().expect("lane0 powinien powstać"),
            pool.acquire().expect("lane1 powinien powstać"),
        ];
        let (mut states, mut kv) = pool
            .take_mtp_pair(leases)
            .expect("para powinna być dostępna");
        for state in &mut states {
            state
                .checkpoint(&stream)
                .expect("checkpoint powinien powstać");
            state.grow(&mut kv).expect("krok draftu powinien powstać");
        }
        let lengths = [states[0].seq.len, states[1].seq.len];
        let checkpoints = [states[0].checkpoint_len(), states[1].checkpoint_len()];

        assert!(validate_mtp_pair_metadata_commit(&states, [0, 1]).is_err());
        assert_eq!([states[0].seq.len, states[1].seq.len], lengths);
        assert_eq!(
            [states[0].checkpoint_len(), states[1].checkpoint_len()],
            checkpoints
        );
        assert!(validate_mtp_pair_metadata_commit(&states, [1, 0]).is_err());
        assert_eq!([states[0].seq.len, states[1].seq.len], lengths);
        assert_eq!(
            [states[0].checkpoint_len(), states[1].checkpoint_len()],
            checkpoints
        );

        let targets = validate_mtp_pair_metadata_commit(&states, [1, 1])
            .expect("poprawny commit obu lane'ów powinien się udać");
        apply_mtp_pair_metadata_commit(&mut states, &mut kv, targets);
        pool.restore_mtp_pair(leases, states, kv)
            .expect("para po commicie powinna wrócić do puli");
    }

    #[test]
    fn blad_restore_pary_kwarantannuje_stany_i_cache() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let mut pool = testowa_pula_mtp(device);
        let leases = [
            pool.acquire().expect("lane0 powinien powstać"),
            pool.acquire().expect("lane1 powinien powstać"),
        ];
        let (states, kv) = pool
            .take_mtp_pair(leases)
            .expect("para powinna być dostępna");

        assert!(pool
            .restore_mtp_pair([leases[0], leases[0]], states, kv)
            .is_err());
        assert!(pool.poisoned.is_some());
        assert_eq!(pool.quarantined_mtp_states.len(), 2);
        assert_eq!(pool.quarantined_mtp_kv.len(), 1);
        assert!(pool.mtp_kv.is_none());
        assert!(pool.acquire().is_err());
    }

    #[test]
    fn blad_rollback_lane_zatruwa_cala_pare() {
        for failed_lane in 0..2 {
            let device: Arc<dyn Device> = CpuDevice::new();
            let stream = device.create_stream().expect("stream CPU powinien powstać");
            let mut pool = testowa_pula_mtp(device.clone());
            let leases = [
                pool.acquire().expect("lane0 powinien powstać"),
                pool.acquire().expect("lane1 powinien powstać"),
            ];
            let (mut states, mut kv) = pool
                .take_mtp_pair(leases)
                .expect("para powinna być dostępna");
            for state in &mut states {
                state
                    .checkpoint(&stream)
                    .expect("checkpoint powinien powstać");
                state.grow(&mut kv).expect("krok draftu powinien powstać");
            }
            states[failed_lane].inject_rollback_failure();

            let rollback = rollback_mtp_pair(&mut states, &mut kv, &stream)
                .expect_err("rollback wskazanego lane powinien się nie udać");
            assert!(rollback.to_string().contains(&format!("lane{failed_lane}")));
            pool.poison(format!("wymuszony błąd propose: {rollback}"));
            assert!(pool.restore_mtp_pair(leases, states, kv).is_err());
            assert!(pool.poisoned.is_some());
            assert_eq!(pool.quarantined_mtp_states.len(), 2);
            assert_eq!(pool.quarantined_mtp_kv.len(), 1);
            assert!(pool.take_mtp_pair(leases).is_err());
        }
    }

    #[test]
    fn blad_checkpointu_propose_lane_zatruwa_cala_pare() {
        for failed_lane in 0..2 {
            let device: Arc<dyn Device> = CpuDevice::new();
            let stream = device.create_stream().expect("stream CPU powinien powstać");
            let mut pool = testowa_pula_mtp(device);
            let leases = [
                pool.acquire().expect("lane0 powinien powstać"),
                pool.acquire().expect("lane1 powinien powstać"),
            ];
            let (mut states, mut kv) = pool
                .take_mtp_pair(leases)
                .expect("para powinna być dostępna");
            states[failed_lane].inject_checkpoint_failure();

            let propose = (|| {
                states[0].checkpoint(&stream)?;
                states[1].checkpoint(&stream)
            })();
            let propose_error = propose.expect_err("checkpoint wskazanego lane powinien zawieść");
            let checkpoints_complete = states.iter().all(|state| state.checkpoint_len().is_some());
            rollback_mtp_pair(&mut states, &mut kv, &stream)
                .expect("utworzony checkpoint drugiego lane powinien się cofnąć");
            assert!(!checkpoints_complete);
            pool.poison(format!(
                "błąd propose przed utworzeniem obu checkpointów: {propose_error}"
            ));
            assert!(pool.restore_mtp_pair(leases, states, kv).is_err());
            assert_eq!(pool.quarantined_mtp_states.len(), 2);
            assert_eq!(pool.quarantined_mtp_kv.len(), 1);
            assert!(pool.acquire().is_err());
        }
    }

    #[test]
    fn blad_eventu_z_udanym_sync_nie_powoduje_wzrostu_puli() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mut pool = HybridStatePool::new(
            device,
            vec![LayerKind::DeltaNet],
            DeltaStateLayout::KeyValue,
            8,
            16,
            None,
            0,
        )
        .expect("pula powinna powstać");

        for _ in 0..64 {
            let lease = pool.acquire().expect("slot powinien wrócić do puli");
            pool.activate(lease, &stream)
                .expect("slot powinien się aktywować");
            let state = pool.active_layers()[0]
                .as_ref()
                .expect("warstwa DeltaNet ma stan");
            pool.device
                .write(&[7; 16], &state.state, 0)
                .expect("zapis stanu powinien się udać");
            pool.finish_release(
                lease,
                Err(ForgeError::Device("wymuszony błąd eventu".into())),
                || Ok(()),
            )
            .expect("synchronizacja powinna bezpiecznie odzyskać slot");
            let mut bytes = [0xff; 16];
            pool.device
                .read(
                    &pool.slots[lease.slot].layers[0]
                        .as_ref()
                        .expect("warstwa DeltaNet ma stan")
                        .state,
                    0,
                    &mut bytes,
                )
                .expect("odczyt stanu powinien się udać");
            assert_eq!(bytes, [0; 16]);
        }

        assert_eq!(pool.slots.len(), 1);
        assert_eq!(pool.free, vec![0]);
        assert!(pool.poisoned.is_none());
    }

    #[test]
    fn podwojny_blad_zwolnienia_zatruwa_pule_i_blokuje_alokacje() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mut pool = HybridStatePool::new(
            device,
            vec![LayerKind::DeltaNet],
            DeltaStateLayout::KeyValue,
            8,
            16,
            None,
            0,
        )
        .expect("pula powinna powstać");
        let lease = pool.acquire().expect("lease powinien powstać");
        pool.activate(lease, &stream)
            .expect("slot powinien się aktywować");

        let result = pool.finish_release(
            lease,
            Err(ForgeError::Device("wymuszony błąd eventu".into())),
            || Err(ForgeError::Device("wymuszony błąd synchronizacji".into())),
        );

        assert!(result.is_err());
        assert!(pool.poisoned.is_some());
        assert!(pool.slots[lease.slot].in_use);
        assert!(pool.free.is_empty());
        assert!(pool.acquire().is_err());
        assert_eq!(pool.slots.len(), 1);
    }

    #[test]
    fn fed_routed_mtp_ponad_i32_konczy_sie_przed_mutacja() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let mut pool = testowa_pula_mtp(device);
        let leases = [
            pool.acquire().expect("lane0 powinien powstać"),
            pool.acquire().expect("lane1 powinien powstać"),
        ];
        let (states, kv) = pool
            .take_mtp_pair(leases)
            .expect("para powinna być dostępna");
        let lengths = [states[0].seq.len, states[1].seq.len];
        let checkpoints = [states[0].checkpoint_len(), states[1].checkpoint_len()];
        let free_pages = kv.free_page_count();
        let result = validate_mtp_routed_inputs(usize::MAX, [u32::MAX, 7], 2, [None, None]);
        assert!(matches!(result, Err(ForgeError::Format(_))));
        assert_eq!([states[0].seq.len, states[1].seq.len], lengths);
        assert_eq!(
            [states[0].checkpoint_len(), states[1].checkpoint_len()],
            checkpoints
        );
        assert_eq!(kv.free_page_count(), free_pages);
        pool.restore_mtp_pair(leases, states, kv)
            .expect("niezmieniona para powinna wrócić do puli");
    }
}
