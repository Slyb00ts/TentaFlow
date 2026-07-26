// ===== File: expert_spill.rs — NVMe backing store for non-resident MoE experts =====
//
// Trzecia warstwa rezydencji. VRAM i przypięta pamięć hosta są adresowalne przez
// kernel, dysk nie jest — ekspert stąd musi najpierw trafić do slotu, więc ta
// warstwa działa jako źródło tylko do odczytu, a eksmisja jest darmowa (wagi się
// nie zmieniają, nie ma czego zapisywać z powrotem).
//
// Opłacalność stoi na zrównolegleniu. Jedna warstwa MoE wybiera `top_k` ekspertów
// razy trzy projekcje, a wszystkie te bloki są znane naraz zaraz po odczycie
// wyboru routera. Wykonane po kolei byłyby sumą opóźnień; wykonane jednym
// zgłoszeniem przez wiele wątków `pread` mieszczą się w czasie najwolniejszego z
// nich. Dlatego API przyjmuje CAŁY komplet chybień, nigdy pojedynczy blok.
//
// Cache stron systemu operacyjnego zostaje włączony celowo: wolny RAM ponad
// budżetem przypiętym staje się dzięki temu darmową czwartą warstwą, a `O_DIRECT`
// by go odciął.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use forge_types::{ForgeError, Result};

/// Ile wątków obsługuje jedno zgłoszenie odczytu. NVMe osiąga pełną
/// przepustowość dopiero przy głębokiej kolejce, a pojedynczy `pread` jest
/// synchroniczny — jeden wątek zostawiłby większość dysku bezczynną.
const READ_THREADS: usize = 8;

/// Położenie jednego eksperta w pliku zrzutu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpillRegion {
    offset: u64,
    len: usize,
}

impl SpillRegion {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Docelowy bufor jednego odczytu: adres hosta wewnątrz slotu rezydentnego.
///
/// Zapis idzie WPROST do przypiętej pamięci slotu, bez bufora pośredniego —
/// dodatkowa kopia podwoiłaby ruch przy odczycie, który i tak jest wąskim
/// gardłem.
pub struct SpillTarget {
    pub region: SpillRegion,
    pub host_ptr: *mut u8,
}

// Każdy cel wskazuje rozłączny fragment przypiętej pamięci przydzielony jednemu
// ekspertowi, a wywołujący gwarantuje, że urządzenie go nie czyta w trakcie.
unsafe impl Send for SpillTarget {}
unsafe impl Sync for SpillTarget {}

/// Plik zrzutu wag ekspertów.
pub struct ExpertSpill {
    file: File,
    path: PathBuf,
    end: Mutex<u64>,
}

impl ExpertSpill {
    /// Zakłada plik zrzutu w `dir`. Plik jest odlinkowany od razu po otwarciu,
    /// więc znika razem z procesem także po zabiciu — nie zostawia śmieci na
    /// dysku, na którym za chwilę zabraknie miejsca.
    pub fn create(dir: &Path, tag: &str) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| {
            ForgeError::Device(format!("nie mogę utworzyć katalogu zrzutu {dir:?}: {e}"))
        })?;
        let path = dir.join(format!("forge-experts-{tag}-{}.bin", std::process::id()));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| ForgeError::Device(format!("nie mogę otworzyć zrzutu {path:?}: {e}")))?;
        std::fs::remove_file(&path).map_err(|e| {
            ForgeError::Device(format!("nie mogę odlinkować zrzutu {path:?}: {e}"))
        })?;
        Ok(Self {
            file,
            path,
            end: Mutex::new(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bajty zapisane do tej pory.
    pub fn len(&self) -> u64 {
        *self.end.lock().expect("licznik zrzutu zatruty")
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Dopisuje bajty jednego eksperta i zwraca jego położenie.
    pub fn append(&self, bytes: &[u8]) -> Result<SpillRegion> {
        let mut end = self.end.lock().expect("licznik zrzutu zatruty");
        let offset = *end;
        self.file
            .write_all_at(bytes, offset)
            .map_err(|e| ForgeError::Device(format!("zapis zrzutu ekspertów: {e}")))?;
        *end = offset + bytes.len() as u64;
        Ok(SpillRegion {
            offset,
            len: bytes.len(),
        })
    }

    /// Wczytuje komplet ekspertów równolegle. Zwraca błąd pierwszego odczytu,
    /// który się nie powiódł — częściowy komplet jest bezużyteczny, bo brakujący
    /// ekspert i tak zatrzymałby warstwę.
    pub fn read_batch(&self, targets: &[SpillTarget]) -> Result<()> {
        if targets.is_empty() {
            return Ok(());
        }
        let threads = READ_THREADS.min(targets.len());
        let failure: Mutex<Option<String>> = Mutex::new(None);
        std::thread::scope(|scope| {
            for shard in 0..threads {
                let file = &self.file;
                let failure = &failure;
                scope.spawn(move || {
                    for target in targets.iter().skip(shard).step_by(threads) {
                        // Wycinek celuje w pamięć slotu, do której nikt inny w
                        // tym momencie nie pisze ani z niej nie czyta.
                        let buf = unsafe {
                            std::slice::from_raw_parts_mut(target.host_ptr, target.region.len)
                        };
                        if let Err(e) = file.read_exact_at(buf, target.region.offset) {
                            let mut slot = failure.lock().expect("rejestr błędów zatruty");
                            if slot.is_none() {
                                *slot = Some(format!("odczyt zrzutu ekspertów: {e}"));
                            }
                            return;
                        }
                    }
                });
            }
        });
        match failure.into_inner().expect("rejestr błędów zatruty") {
            Some(message) => Err(ForgeError::Device(message)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Odczyt wsadowy musi odtworzyć bajty każdego eksperta u siebie — test
    /// używa wzorca zależnego od indeksu, żeby przestawienie celów nie przeszło.
    #[test]
    fn batch_read_restores_every_region() {
        let dir = std::env::temp_dir().join("forge-spill-test");
        let spill = ExpertSpill::create(&dir, "batch").unwrap();
        let n = 17usize;
        let len = 4096usize;
        let sources: Vec<Vec<u8>> = (0..n)
            .map(|e| (0..len).map(|i| ((e * 31 + i * 7) % 251) as u8).collect())
            .collect();
        let regions: Vec<SpillRegion> = sources
            .iter()
            .map(|bytes| spill.append(bytes).unwrap())
            .collect();

        let mut dest = vec![vec![0u8; len]; n];
        let targets: Vec<SpillTarget> = dest
            .iter_mut()
            .zip(&regions)
            .map(|(slot, region)| SpillTarget {
                region: *region,
                host_ptr: slot.as_mut_ptr(),
            })
            .collect();
        spill.read_batch(&targets).unwrap();
        drop(targets);
        for (e, (got, want)) in dest.iter().zip(&sources).enumerate() {
            assert_eq!(got, want, "ekspert {e} wrócił z dysku zniekształcony");
        }
    }

    /// Pusty komplet nie może zawieść ani niczego dotknąć.
    #[test]
    fn empty_batch_is_a_no_op() {
        let dir = std::env::temp_dir().join("forge-spill-test");
        let spill = ExpertSpill::create(&dir, "empty").unwrap();
        spill.read_batch(&[]).unwrap();
        assert!(spill.is_empty());
    }
}
