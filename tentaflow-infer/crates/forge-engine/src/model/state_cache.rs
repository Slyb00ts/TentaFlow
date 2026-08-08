// ===== File: model/state_cache.rs — pula stanow DeltaNet i cache checkpointow =====
//
// Sekwencja hybrydowa niesie stan rekurencyjny, ktorego stronicowany KV nie
// opisuje: okno splotu i macierz stanu kazdej warstwy DeltaNet. Zyje on w
// slotach puli, po jednym na sekwencje. Prefiks wspoldzielony potrzebuje tego
// samego slotu w drugiej roli — jako CHECKPOINT pozycji, w ktorej konczy sie
// wspoldzielony prefiks — wiec obie role biora z JEDNEJ puli. Dzieki temu nie
// ma proporcji do strojenia: rosnaca wspolbieznosc odbiera sloty cache'owi.
use super::*;

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
}

impl HybridStatePool {
    pub(crate) fn new(
        device: Arc<dyn Device>,
        layer_kinds: Vec<LayerKind>,
        layout: DeltaStateLayout,
        conv_bytes: usize,
        state_bytes: usize,
        mtp_config: Option<(KvConfig, usize, usize)>,
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
        let additional = slots.saturating_sub(self.slots.len());
        if additional == 0 {
            return Ok(());
        }
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
        let weights_per_slot = reserve(self.conv_bytes)?
            .checked_add(reserve(self.state_bytes)?)
            .and_then(|bytes| bytes.checked_mul(delta_layers))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie rozmiaru slotu SSM".into()))?;
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
        self.free.extend(first..slots);
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
