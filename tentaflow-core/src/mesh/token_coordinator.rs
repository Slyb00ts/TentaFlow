// =============================================================================
// Plik: mesh/token_coordinator.rs
// Opis: Deterministyczny wybor koordynatora dzierzaw tokenow przez rendezvous
//       hashing (HRW). Kazdy wezel liczy ten sam wynik z tego samego zbioru
//       kandydatow — bez wymiany komunikatow elekcji.
// =============================================================================

/// Deterministyczny wybor koordynatora metoda rendezvous-hash (HRW). Kazdy wezel
/// niezaleznie liczy tego samego zwyciezce z tego samego zbioru kandydatow — brak
/// komunikatow elekcji. Gdy zbior sie zmienia, zwyciezca deterministycznie
/// "re-kolapsuje" do nowego.
///
/// Wynik = kandydat o maksymalnym `blake3("{role_key}|{candidate}")`. Hash
/// porownujemy jako 32-bajtowa tablice big-endian; remis (teoretycznie niemozliwy
/// dla blake3, ale gwarantuje pelny determinizm) rozstrzyga `node_id` leksykalnie.
/// Pusty zbior → `None`. Wywolujacy podaje `role_key = format!("token-coord|{org_id}")`.
pub fn elect_coordinator(role_key: &str, candidates: &[String]) -> Option<String> {
    let mut best: Option<(&str, [u8; 32])> = None;
    for candidate in candidates {
        let score = *blake3::hash(format!("{role_key}|{candidate}").as_bytes()).as_bytes();
        let take = match best {
            None => true,
            Some((best_id, best_score)) => match score.cmp(&best_score) {
                std::cmp::Ordering::Greater => true,
                // Remis po hashu rozstrzygamy leksykalnie po node_id — total order.
                std::cmp::Ordering::Equal => candidate.as_str() > best_id,
                std::cmp::Ordering::Less => false,
            },
        };
        if take {
            best = Some((candidate.as_str(), score));
        }
    }
    best.map(|(id, _)| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_set_yields_none() {
        assert_eq!(elect_coordinator("token-coord|org-1", &[]), None);
    }

    #[test]
    fn single_node_wins() {
        let set = nodes(&["node-a"]);
        assert_eq!(
            elect_coordinator("token-coord|org-1", &set),
            Some("node-a".to_string())
        );
    }

    #[test]
    fn deterministic_regardless_of_order() {
        let role = "token-coord|org-1";
        let ordered = nodes(&["node-a", "node-b", "node-c", "node-d", "node-e"]);
        let winner = elect_coordinator(role, &ordered).unwrap();

        // Powtarzalnosc: te same wejscia → ten sam wynik.
        for _ in 0..5 {
            assert_eq!(elect_coordinator(role, &ordered), Some(winner.clone()));
        }

        // Niezaleznosc od kolejnosci: kazda permutacja daje tego samego zwyciezce.
        let permutations = [
            nodes(&["node-e", "node-d", "node-c", "node-b", "node-a"]),
            nodes(&["node-c", "node-a", "node-e", "node-b", "node-d"]),
            nodes(&["node-b", "node-e", "node-a", "node-d", "node-c"]),
        ];
        for perm in &permutations {
            assert_eq!(elect_coordinator(role, perm), Some(winner.clone()));
        }
    }

    #[test]
    fn all_nodes_agree_on_winner() {
        // Kazdy wezel liczy elekcje z pelnego zbioru — wszyscy musza wskazac
        // tego samego koordynatora niezaleznie od tego, ktory liczy.
        let role = "token-coord|org-42";
        let set = nodes(&["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]);
        let expected = elect_coordinator(role, &set).unwrap();
        for _viewpoint in &set {
            assert_eq!(elect_coordinator(role, &set), Some(expected.clone()));
        }
    }

    #[test]
    fn removing_winner_re_collapses_to_stable_new_winner() {
        let role = "token-coord|org-7";
        let full = nodes(&["n1", "n2", "n3", "n4", "n5", "n6"]);
        let winner = elect_coordinator(role, &full).unwrap();

        // Usuniecie zwyciezcy → nowy, stabilny zwyciezca (powtarzalny).
        let without_winner: Vec<String> = full.iter().filter(|n| **n != winner).cloned().collect();
        let new_winner = elect_coordinator(role, &without_winner).unwrap();
        assert_ne!(new_winner, winner);
        assert_eq!(
            elect_coordinator(role, &without_winner),
            Some(new_winner.clone())
        );

        // Usuniecie dowolnego NIE-zwyciezcy (i nie nowego zwyciezcy) nie zmienia
        // wyniku — HRW jest stabilny przy zmianach nie dotykajacych szczytu.
        for victim in &full {
            if *victim == winner || *victim == new_winner {
                continue;
            }
            let reduced: Vec<String> = full.iter().filter(|n| **n != *victim).cloned().collect();
            assert_eq!(elect_coordinator(role, &reduced), Some(winner.clone()));
        }
    }

    #[test]
    fn distinct_role_keys_can_pick_distinct_winners() {
        // Rozne org-id (rozny role_key) rozkladaja koordynacje na rozne wezly —
        // nie ma jednego globalnego punktu zapchania przy wielu organizacjach.
        let set = nodes(&["w1", "w2", "w3", "w4", "w5"]);
        let mut winners = std::collections::HashSet::new();
        for org in 0..20 {
            let role = format!("token-coord|org-{org}");
            winners.insert(elect_coordinator(&role, &set).unwrap());
        }
        assert!(
            winners.len() > 1,
            "HRW powinno rozkladac role na wiele wezlow"
        );
    }
}
