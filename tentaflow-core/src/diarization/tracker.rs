// =============================================================================
// Plik: diarization/tracker.rs
// Opis: Online speaker clustering. Per mowca przechowujemy okno N ostatnich
//       embeddingow (L2-znormalizowanych) zamiast pojedynczego centroidu —
//       nowy embedding jest dopasowywany do *maksymalnej* similarity po
//       wszystkich embeddingach w oknach wszystkich mowcow. To jest znacznie
//       bardziej odporne na wariancje akustyczna tego samego glosu (glosno
//       vs cicho, rozna energia, zmiana mikrofonu) niz klasyczny centroid.
// =============================================================================

use super::embedding::cosine_similarity;
use std::collections::VecDeque;

/// Maksymalna liczba ostatnich embeddingow przechowywanych per mowca.
/// Wiecej = wiecej pamieci i wiecej porownan, ale lepsza odpornosc.
const EMBEDDINGS_PER_SPEAKER: usize = 8;

struct Speaker {
    label: String,
    /// Okno ostatnich embeddingow (L2-znormalizowane). Nowe dodawane na koniec,
    /// najstarsze usuwane gdy rozmiar > EMBEDDINGS_PER_SPEAKER.
    recent: VecDeque<Vec<f32>>,
    /// Calkowita liczba przypisanych wypowiedzi (dla statystyk)
    count: usize,
}

pub struct SpeakerTracker {
    speakers: Vec<Speaker>,
    similarity_threshold: f32,
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

    pub fn reset(&mut self) {
        self.speakers.clear();
    }

    /// Dopasowuje embedding do istniejacego speakera albo tworzy nowego.
    /// Uzywa max-similarity po wszystkich embeddingach w oknie kazdego mowcy.
    pub fn track(&mut self, embedding: &[f32]) -> String {
        let normalized = l2_normalize(embedding);

        // Znajdz speakera z NAJWIEKSZA max-similarity do jego okna
        let mut best_idx: Option<usize> = None;
        let mut best_sim: f32 = -1.0;

        for (i, spk) in self.speakers.iter().enumerate() {
            let max_sim_for_speaker = spk
                .recent
                .iter()
                .map(|e| cosine_similarity(&normalized, e))
                .fold(f32::NEG_INFINITY, f32::max);
            if max_sim_for_speaker > best_sim {
                best_sim = max_sim_for_speaker;
                best_idx = Some(i);
            }
        }

        match best_idx {
            Some(idx) if best_sim >= self.similarity_threshold => {
                let spk = &mut self.speakers[idx];
                spk.recent.push_back(normalized);
                while spk.recent.len() > EMBEDDINGS_PER_SPEAKER {
                    spk.recent.pop_front();
                }
                spk.count += 1;
                tracing::debug!(
                    speaker = %spk.label,
                    similarity = best_sim,
                    count = spk.count,
                    window = spk.recent.len(),
                    "Speaker matched"
                );
                spk.label.clone()
            }
            _ if self.speakers.len() < self.max_speakers => {
                let label = format!("SPEAKER_{:02}", self.speakers.len());
                let mut recent = VecDeque::with_capacity(EMBEDDINGS_PER_SPEAKER);
                recent.push_back(normalized);
                self.speakers.push(Speaker {
                    label: label.clone(),
                    recent,
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
                let spk = &mut self.speakers[idx];
                spk.recent.push_back(normalized);
                while spk.recent.len() > EMBEDDINGS_PER_SPEAKER {
                    spk.recent.pop_front();
                }
                spk.count += 1;
                tracing::debug!(
                    speaker = %spk.label,
                    similarity = best_sim,
                    "Speaker matched (max_speakers reached, forced)"
                );
                spk.label.clone()
            }
            None => "SPEAKER_UNKNOWN".to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.speakers.len()
    }
}

fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-12 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
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
        let label = t.track(&emb3);
        assert!(label == "SPEAKER_00" || label == "SPEAKER_01");
    }

    /// Regresja: ten sam glos z variancja akustyczna nie powinien byc
    /// sklasyfikowany jako nowy mowca gdy jeden z historycznych embeddingow
    /// jest mu bliski (max-similarity > threshold dzieki sliding window).
    #[test]
    fn test_acoustic_variance_stays_same_speaker() {
        let mut t = SpeakerTracker::new(0.5, 10);
        // Trzy warianty tego samego glosu, przesuwajace sie w feature space,
        // ale zawsze co najmniej jeden poprzedni ma cos > 0.5 z nowym:
        //   cos(e1, e2) = 0.8
        //   cos(e1, e3) ~ 0.33 (bezposrednio za daleko)
        //   cos(e2, e3) ~ 0.72 (ale e2 jest w oknie → match)
        let e1 = normalize(vec![1.0, 0.0, 0.0, 0.0]);
        let e2 = normalize(vec![0.8, 0.6, 0.0, 0.0]);
        let e3 = normalize(vec![0.3, 0.7, 0.5, 0.0]);
        assert_eq!(t.track(&e1), "SPEAKER_00");
        assert_eq!(t.track(&e2), "SPEAKER_00");
        assert_eq!(t.track(&e3), "SPEAKER_00");
    }
}
