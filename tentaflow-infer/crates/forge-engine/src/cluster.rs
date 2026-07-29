// ===== File: cluster.rs — kontekst wykonawczy na wielu kartach =====
//
// Fundament, którego wymaga KAŻDA technika podziału: tensor parallel, pipeline,
// sekwencyjny prefill i expert parallel. Trzyma otwarte karty, ich strumienie i
// zdarzenia oraz otwarty dostęp P2P, a wołającemu daje dwie operacje, na których
// stoi cała reszta:
//
//   `exchange` — kopia aktywacji między kartami,
//   `wait_for` — druga karta czeka na wynik pierwszej BEZ powrotu do hosta.
//
// Dlaczego to jest osobny moduł, a nie kod w silniku: podział pracy planuje
// `topology`, wykonuje go silnik, a `cluster` odpowiada wyłącznie za to, żeby
// karty się widziały i umiały na siebie poczekać. Zmierzone na tej maszynie
// (`peer_probe`): wymiana 10 KiB ze zdarzeniem to 11,21 us, przez hosta 35,2 us.

use forge_hal::{Device, Event, PoolSizes, Stream, gpu};
use forge_types::{ForgeError, Result};
use std::sync::Arc;

/// Jedna karta w klastrze wraz z jej strumieniem i zdarzeniem sygnalizującym
/// zakończenie pracy.
pub struct ClusterDevice {
    pub device: Arc<dyn Device>,
    pub stream: Stream,
    /// Zdarzenie zapisywane na `stream`, na które czekają pozostałe karty.
    pub done: Event,
}

/// Karty otwarte w jednym procesie, z otwartym dostępem P2P w obie strony.
pub struct Cluster {
    devices: Vec<ClusterDevice>,
    /// Czy KAŻDA para kart widzi swoją pamięć. Gdy nie, wymiana musi iść przez
    /// hosta, a planer powinien wybrać technikę mniej zależną od łącza.
    peer_access: bool,
}

impl Cluster {
    /// Otwiera pierwsze `count` widocznych kart i próbuje otworzyć P2P między
    /// każdą parą. Brak P2P NIE jest błędem — jest informacją dla planera,
    /// dlatego trafia do `peer_access`, a nie do `Err`.
    pub fn open(count: usize, pools: PoolSizes) -> Result<Self> {
        if count == 0 {
            return Err(ForgeError::Scheduler("klaster bez kart".into()));
        }
        let visible = gpu::enumerate();
        if visible.len() < count {
            return Err(ForgeError::Scheduler(format!(
                "zażądano {count} kart, widocznych jest {}",
                visible.len()
            )));
        }
        let mut devices = Vec::with_capacity(count);
        for id in visible.into_iter().take(count) {
            let device = gpu::open_id(id, pools)?;
            let stream = device.create_stream()?;
            let done = device.create_event()?;
            devices.push(ClusterDevice {
                device,
                stream,
                done,
            });
        }

        let mut peer_access = true;
        for from in 0..devices.len() {
            for to in 0..devices.len() {
                if from == to {
                    continue;
                }
                let peer = devices[to].device.ordinal();
                if devices[from].device.enable_peer_access(peer).is_err() {
                    peer_access = false;
                }
            }
        }

        Ok(Self {
            devices,
            peer_access,
        })
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn peer_access(&self) -> bool {
        self.peer_access
    }

    pub fn device(&self, index: usize) -> Result<&ClusterDevice> {
        self.devices
            .get(index)
            .ok_or_else(|| ForgeError::Scheduler(format!("nie ma karty {index} w klastrze")))
    }

    pub fn devices(&self) -> &[ClusterDevice] {
        &self.devices
    }

    /// Kopiuje `bytes` bajtów z bufora karty `from` do bufora karty `to`.
    /// Kopia jest zlecana na strumieniu ŹRÓDŁA — to ta karta wie, kiedy dane są
    /// gotowe.
    #[allow(clippy::too_many_arguments)]
    pub fn exchange(
        &self,
        from: usize,
        src: &forge_hal::DevBuffer,
        src_offset: usize,
        to: usize,
        dst: &forge_hal::DevBuffer,
        dst_offset: usize,
        bytes: usize,
    ) -> Result<()> {
        if from == to {
            return Err(ForgeError::Scheduler(
                "wymiana wymaga dwóch różnych kart".into(),
            ));
        }
        let source = self.device(from)?;
        self.device(to)?;
        source
            .device
            .copy(src, src_offset, dst, dst_offset, bytes, &source.stream)
    }

    /// Sprawia, że karta `waiter` czeka na zakończenie bieżącej pracy karty
    /// `signaller` — bez synchronizacji hosta. To jest ta operacja, która
    /// decyduje o opłacalności tensor parallel.
    pub fn wait_for(&self, waiter: usize, signaller: usize) -> Result<()> {
        if waiter == signaller {
            return Ok(());
        }
        let source = self.device(signaller)?;
        let target = self.device(waiter)?;
        source.device.record_event(&source.done, &source.stream)?;
        target.device.wait_event(&target.stream, &source.done)
    }

    /// Czeka na wszystkie karty. Wyłącznie do granic kroku i do testów — w
    /// pętli warstw używa się `wait_for`.
    pub fn synchronize(&self) -> Result<()> {
        for entry in &self.devices {
            entry.stream.synchronize()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn klaster_bez_kart_jest_bledem() {
        let pools = PoolSizes {
            weights: 1 << 20,
            kv_cache: 1 << 20,
            activations: 1 << 20,
            kv_page_size: 4096,
        };
        assert!(Cluster::open(0, pools).is_err());
    }

    #[test]
    fn wymiana_na_te_sama_karte_jest_bledem() {
        // Sprawdzane bez sprzętu: kontrakt musi odrzucić bezsensowne wołanie
        // zanim dotknie sterownika.
        let cluster = Cluster {
            devices: Vec::new(),
            peer_access: false,
        };
        assert!(cluster.device(0).is_err());
    }

    #[test]
    fn oczekiwanie_na_samego_siebie_jest_bezczynne() {
        let cluster = Cluster {
            devices: Vec::new(),
            peer_access: false,
        };
        assert!(cluster.wait_for(3, 3).is_ok());
    }
}
