// =============================================================================
// Plik: bench.rs
// Opis: Wczytuje jednoznaczne wejście tokenów benchmarku i oblicza statystyki.
// Przykład: let input = TokenInput::read_u32le(path)?;
// =============================================================================

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

pub struct TokenInput {
    pub ids: Vec<u32>,
    pub sha256: String,
}

impl TokenInput {
    pub fn read_u32le(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("nie można odczytać {}", path.display()))?;
        Self::from_u32le(&bytes)
            .with_context(|| format!("niepoprawny plik tokenów {}", path.display()))
    }

    fn from_u32le(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            bail!("plik .u32le nie może być pusty");
        }
        if !bytes.len().is_multiple_of(4) {
            bail!("rozmiar pliku .u32le musi być wielokrotnością 4 bajtów");
        }
        let ids = bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("blok ma cztery bajty")))
            .collect();
        Ok(Self {
            ids,
            sha256: sha256_bytes(bytes),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Distribution {
    pub p10: f64,
    pub median: f64,
    pub p90: f64,
}

impl Distribution {
    pub fn from_samples(samples: &[f64]) -> Result<Self> {
        if samples.is_empty() {
            bail!("statystyka wymaga co najmniej jednej próbki");
        }
        if samples.iter().any(|value| !value.is_finite()) {
            bail!("próbki statystyki muszą być skończone");
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        Ok(Self {
            p10: nearest_rank(&sorted, 0.10),
            median: nearest_rank(&sorted, 0.50),
            p90: nearest_rank(&sorted, 0.90),
        })
    }
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let file =
        File::open(path).with_context(|| format!("nie można otworzyć {}", path.display()))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("nie można haszować {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn sha256_ids(ids: &[u32]) -> String {
    let mut hasher = Sha256::new();
    for id in ids {
        hasher.update(id.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn nearest_rank(sorted: &[f64], quantile: f64) -> f64 {
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::{Distribution, TokenInput};

    #[test]
    fn parser_odczytuje_u32_w_kolejnosci_little_endian() {
        let bytes = [1u32, 0x0102_0304, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();

        let input = TokenInput::from_u32le(&bytes).expect("wejście powinno być poprawne");

        assert_eq!(input.ids, [1, 0x0102_0304, u32::MAX]);
    }

    #[test]
    fn parser_odrzuca_ucięty_token() {
        let result = TokenInput::from_u32le(&[1, 2, 3]);

        assert!(result.is_err());
    }

    #[test]
    fn parser_odrzuca_pusty_plik() {
        let result = TokenInput::from_u32le(&[]);

        assert!(result.is_err());
    }

    #[test]
    fn parser_liczy_sha256_surowych_bajtów() {
        let input =
            TokenInput::from_u32le(&1u32.to_le_bytes()).expect("wejście powinno być poprawne");

        assert_eq!(
            input.sha256,
            "67abdd721024f0ff4e0b3f4c2fc13bc5bad42d0b7851d456d88d203d15aaa450"
        );
    }

    #[test]
    fn statystyki_używają_najbliższej_rangi() {
        let samples = [9.0, 1.0, 5.0, 2.0, 8.0, 3.0, 7.0, 4.0, 6.0, 10.0];

        let distribution =
            Distribution::from_samples(&samples).expect("próbki powinny być poprawne");

        assert_eq!(
            distribution,
            Distribution {
                p10: 1.0,
                median: 5.0,
                p90: 9.0,
            }
        );
    }

    #[test]
    fn statystyki_odrzucają_nan() {
        let result = Distribution::from_samples(&[1.0, f64::NAN]);

        assert!(result.is_err());
    }
}
