// =============================================================================
// Plik: speculation/config.rs
// Opis: Typowana konfiguracja oraz fabryka stanów dekodowania spekulacyjnego.
// Przykład: SpeculationCoordinator::new(SpeculativeConfig::ngram(16)?)
// =============================================================================

use forge_types::{ForgeError, Result};

use super::{CascadeComposer, NgramProposer, Proposer, SpeculativeState};
use crate::model::MAX_SPEC_DRAFT;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProposerKind {
    Ngram,
    DraftModel,
    Mtp,
    Eagle,
    DFlash,
    DSpark,
}

/// Określa właściciela stanu i wykonania spekulacji.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeculationKind {
    Off,
    HostProposer,
    NativeMtp,
    NativeMtpNgram,
}

impl ProposerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ngram => "ngram",
            Self::DraftModel => "draft-model",
            Self::Mtp => "mtp",
            Self::Eagle => "eagle",
            Self::DFlash => "dflash",
            Self::DSpark => "dspark",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeculativeConfig {
    proposers: Vec<ProposerKind>,
    draft_tokens: usize,
}

impl SpeculativeConfig {
    pub fn off() -> Self {
        Self {
            proposers: Vec::new(),
            draft_tokens: 0,
        }
    }

    pub fn ngram(draft_tokens: usize) -> Result<Self> {
        Self::chain(vec![ProposerKind::Ngram], draft_tokens)
    }

    pub fn chain(proposers: Vec<ProposerKind>, draft_tokens: usize) -> Result<Self> {
        if !(1..=MAX_SPEC_DRAFT).contains(&draft_tokens) {
            return Err(ForgeError::Unsupported(format!(
                "speculative draft budget must be in 1..={MAX_SPEC_DRAFT}"
            )));
        }
        if proposers.is_empty() {
            return Err(ForgeError::Unsupported(
                "enabled speculative configuration requires at least one proposer".into(),
            ));
        }
        if proposers.contains(&ProposerKind::Mtp)
            && proposers != [ProposerKind::Mtp]
            && proposers != [ProposerKind::Mtp, ProposerKind::Ngram]
        {
            return Err(ForgeError::Unsupported(
                "native MTP może działać samodzielnie albo z priorytetowym n-gramem".into(),
            ));
        }
        for (index, kind) in proposers.iter().enumerate() {
            if proposers[..index].contains(kind) {
                return Err(ForgeError::Unsupported(format!(
                    "speculative proposer '{}' is duplicated",
                    kind.as_str()
                )));
            }
            if *kind == ProposerKind::Ngram && index + 1 != proposers.len() {
                return Err(ForgeError::Unsupported(
                    "ngram proposer must be the final cascade extension".into(),
                ));
            }
        }
        if proposers.contains(&ProposerKind::Mtp) && !matches!(draft_tokens, 2 | 3) {
            return Err(ForgeError::Unsupported(
                "native MTP draft budget must be 2 or 3".into(),
            ));
        }
        Ok(Self {
            proposers,
            draft_tokens,
        })
    }

    pub fn is_enabled(&self) -> bool {
        !self.proposers.is_empty()
    }

    pub fn proposers(&self) -> &[ProposerKind] {
        &self.proposers
    }

    pub fn draft_tokens(&self) -> usize {
        self.draft_tokens
    }

    /// Rozróżnia wyłączoną spekulację, proposer hostowy i natywne MTP modelu.
    pub fn kind(&self) -> SpeculationKind {
        match self.proposers.as_slice() {
            [] => SpeculationKind::Off,
            [ProposerKind::Mtp] => SpeculationKind::NativeMtp,
            [ProposerKind::Mtp, ProposerKind::Ngram] => SpeculationKind::NativeMtpNgram,
            _ => SpeculationKind::HostProposer,
        }
    }
}

pub struct SpeculationCoordinator {
    config: SpeculativeConfig,
}

impl SpeculationCoordinator {
    pub fn new(config: SpeculativeConfig) -> Result<Self> {
        if config.kind() == SpeculationKind::HostProposer {
            for &kind in config.proposers() {
                Self::build_proposer(kind)?;
            }
        }
        Ok(Self { config })
    }

    fn build_proposer(kind: ProposerKind) -> Result<Box<dyn Proposer>> {
        match kind {
            ProposerKind::Ngram => Ok(Box::new(NgramProposer::with_min_gram(3))),
            _ => Err(ForgeError::Unsupported(format!(
                "speculative proposer '{}' is not implemented",
                kind.as_str()
            ))),
        }
    }

    pub fn new_state(&self, history: &[u32]) -> Result<Option<SpeculativeState>> {
        let proposers = match self.config.kind() {
            SpeculationKind::HostProposer => self
                .config
                .proposers()
                .iter()
                .map(|&kind| Self::build_proposer(kind))
                .collect::<Result<Vec<_>>>()?,
            SpeculationKind::NativeMtpNgram => {
                vec![Self::build_proposer(ProposerKind::Ngram)?]
            }
            SpeculationKind::Off | SpeculationKind::NativeMtp => return Ok(None),
        };
        let composer = CascadeComposer::new(proposers);
        let mut state = SpeculativeState::new(composer);
        state.observe_all(history);
        Ok(Some(state))
    }

    pub fn config(&self) -> &SpeculativeConfig {
        &self.config
    }
}
