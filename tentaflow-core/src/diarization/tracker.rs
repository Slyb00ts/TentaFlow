// =============================================================================
// Plik: diarization/tracker.rs
// Opis: Incremental speaker clustering — dla kazdego nowego embeddingu
//       sprawdza czy pasuje do istniejacego centroidu (cosine similarity),
//       jesli tak: update centroid moving average; jesli nie: stworz nowego
//       SPEAKER_XX. Stan resetowany przy zmianie meetingu.
// =============================================================================

use super::embedding::cosine_similarity;

/// Jeden zidentyfikowany mowca
struct Speaker {
    /// Etykieta (np. "SPEAKER_00")
    label: String,
    /// Sredni embedding (centroid) po wszystkich wypowiedziach
    centroid: Vec<f32>,
    /// Liczba wypowiedzi przypisanych do tego speakera (do running average)
    count: usize,
}

/// Incremental speaker tracker
pub struct SpeakerTracker {
    speakers: Vec<Speaker>,
    /// Prog similarity ponizej ktorego zakladamy ze to nowy mowca
    similarity_threshold: f32,
    /// Maksymalna liczba mowcow (dalej przypisujemy do najblizszego)
    max_speakers: usize,
}

impl SpeakerTracker {
    pub fn new(similarity_threshold: f32, max_speakers: usize) -> Self {
        Self {
            speakers: Vec::new(),
            similarity_threshold,
            max_speakers,
        }
    }

    /// Resetuje stan — nowy meeting, nowi speakerzy od SPEAKER_00
    pub fn reset(&mut self) {
        self.speakers.clear();
    }

    /// Dopasowuje embedding do istniejacego speakera albo tworzy nowego.
    /// Zwraca etykiete speakera (np. "SPEAKER_00").
    pub fn track(&mut self, embedding: &[f32]) -> String {
        // Znajdz najblizszego speakera
        let mut best_idx: Option<usize> = None;
        let mut best_sim: f32 = -1.0;

        for (i, spk) in self.speakers.iter().enumerate() {
            let sim = cosine_similarity(embedding, &spk.centroid);
            if sim > best_sim {
                best_sim = sim;
                best_idx = Some(i);
            }
        }

        // Dopasowanie
        match best_idx {
            Some(idx) if best_sim >= self.similarity_threshold => {
                // Aktualizuj centroid (running average w przestrzeni raw embeddingow,
                // bez L2 renormalizacji — cosine_similarity i tak auto-normalizuje,
                // a mieszanie znormalizowanego centroidu z raw embeddingiem psuje srednia).
                let spk = &mut self.speakers[idx];
                let n = spk.count as f32;
                for (i, v) in spk.centroid.iter_mut().enumerate() {
                    *v = (*v * n + embedding[i]) / (n + 1.0);
                }
                spk.count += 1;
                tracing::debug!(
                    speaker = %spk.label,
                    similarity = best_sim,
                    count = spk.count,
                    "Speaker matched"
                );
                spk.label.clone()
            }
            _ if self.speakers.len() < self.max_speakers => {
                // Nowy mowca
                let label = format!("SPEAKER_{:02}", self.speakers.len());
                self.speakers.push(Speaker {
                    label: label.clone(),
                    centroid: embedding.to_vec(),
                    count: 1,
                });
                tracing::info!(
                    speaker = %label,
                    best_sim_to_existing = best_sim,
                    "Nowy speaker utworzony"
                );
                label
            }
            Some(idx) => {
                // Limit osiagniety — przypisz do najblizszego mimo nizszej similarity
                let spk = &mut self.speakers[idx];
                spk.count += 1;
                tracing::debug!(
                    speaker = %spk.label,
                    similarity = best_sim,
                    "Speaker matched (max_speakers reached, forced)"
                );
                spk.label.clone()
            }
            None => {
                // Pusty tracker + limit 0 — niemozliwe, ale dla bezpieczenstwa
                "SPEAKER_UNKNOWN".to_string()
            }
        }
    }

    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.speakers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize(v: Vec<f32>) -> Vec<f32> {
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn test_first_speaker_is_00() {
        let mut t = SpeakerTracker::new(0.65, 10);
        let emb = normalize(vec![1.0, 0.0, 0.0, 0.5]);
        assert_eq!(t.track(&emb), "SPEAKER_00");
    }

    #[test]
    fn test_similar_embedding_matches_same_speaker() {
        let mut t = SpeakerTracker::new(0.65, 10);
        let emb1 = normalize(vec![1.0, 0.0, 0.0, 0.5]);
        let emb2 = normalize(vec![0.95, 0.05, 0.1, 0.55]);
        assert_eq!(t.track(&emb1), "SPEAKER_00");
        assert_eq!(t.track(&emb2), "SPEAKER_00");
    }

    #[test]
    fn test_different_embedding_creates_new_speaker() {
        let mut t = SpeakerTracker::new(0.65, 10);
        let emb1 = normalize(vec![1.0, 0.0, 0.0, 0.0]);
        let emb2 = normalize(vec![0.0, 0.0, 0.0, 1.0]);
        assert_eq!(t.track(&emb1), "SPEAKER_00");
        assert_eq!(t.track(&emb2), "SPEAKER_01");
    }

    #[test]
    fn test_reset_starts_over() {
        let mut t = SpeakerTracker::new(0.65, 10);
        let emb = normalize(vec![1.0, 0.0, 0.0, 0.0]);
        t.track(&emb);
        t.reset();
        assert_eq!(t.track(&emb), "SPEAKER_00");
    }

    #[test]
    fn test_max_speakers_limit() {
        let mut t = SpeakerTracker::new(0.9, 2);
        let emb1 = normalize(vec![1.0, 0.0, 0.0, 0.0]);
        let emb2 = normalize(vec![0.0, 1.0, 0.0, 0.0]);
        let emb3 = normalize(vec![0.0, 0.0, 1.0, 0.0]);
        assert_eq!(t.track(&emb1), "SPEAKER_00");
        assert_eq!(t.track(&emb2), "SPEAKER_01");
        // Trzeci rozny ale limit 2 — dopasuj do najblizszego
        let label = t.track(&emb3);
        assert!(label == "SPEAKER_00" || label == "SPEAKER_01");
    }
}
