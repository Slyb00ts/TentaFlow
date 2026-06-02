// =============================================================================
// Plik: sync/resource_id.rs
// Opis: Kanoniczny kodek resource_id Sync Ledgera. resource_id jest albo
//       globalnie unikalny (UUID), albo kompozytem z prefiksem scope
//       (org_id, addon_id), tak by klucze roznych orgow nigdy sie nie zlewaly.
// =============================================================================

/// Separator skladnikow kompozytowego resource_id. Unit Separator (US, 0x1F)
/// oddziela dlugosc segmentu od jego tresci. Sam separator moze legalnie
/// wystapic w danych domenowych, dlatego kazdy segment poprzedzamy jego
/// dlugoscia w bajtach — kodowanie jest wtedy injektywne niezaleznie od tego,
/// jakie znaki (lacznie z US) zawieraja segmenty.
pub const RESOURCE_ID_SEP: char = '\u{1f}';

/// Kanoniczna, odwracalna budowa resource_id z czesci. Kazdy segment jest
/// zapisany jako `len<US>bytes`, gdzie `len` to liczba bajtow UTF-8 segmentu.
/// Length-prefix sprawia, ze granice segmentow sa jednoznaczne i zadne dwa
/// rozne wektory wejsciowe nie daja tego samego klucza (injektywnosc).
pub fn composite_resource_id(parts: &[&str]) -> String {
    let mut out = String::new();
    for part in parts {
        out.push_str(&part.len().to_string());
        out.push(RESOURCE_ID_SEP);
        out.push_str(part);
    }
    out
}

/// resource_id zasobu ACL, scope'owany przez org_id (i addon_id). Bez org_id
/// dwie organizacje z tym samym addon_id/resource_type/resource_id zmapowalyby
/// sie na ten sam klucz ledgera i nadpisywaly swoje wpisy.
pub fn scoped_acl_resource_id(
    org_id: &str,
    addon_id: &str,
    resource_type: &str,
    resource_id: &str,
) -> String {
    composite_resource_id(&[org_id, addon_id, resource_type, resource_id])
}

/// resource_id jawnego udostepnienia, scope'owany przez org_id (i addon_id).
#[allow(clippy::too_many_arguments)]
pub fn scoped_explicit_share_resource_id(
    org_id: &str,
    addon_id: &str,
    resource_type: &str,
    resource_id: &str,
    subject_type: &str,
    subject_id: &str,
    action: &str,
) -> String {
    composite_resource_id(&[
        org_id,
        addon_id,
        resource_type,
        resource_id,
        subject_type,
        subject_id,
        action,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_uses_length_prefixed_segments() {
        let id = composite_resource_id(&["a", "bb", "c"]);
        assert_eq!(id, format!("1{0}a2{0}bb1{0}c", RESOURCE_ID_SEP));
    }

    #[test]
    fn composite_is_injective_even_when_segments_contain_separator() {
        // Reviewer's collision case: a raw-separator codec maps both inputs to
        // the same key. Length-prefixing must keep them distinct.
        let left =
            composite_resource_id(&["org", &format!("addon{RESOURCE_ID_SEP}type"), "res", "id"]);
        let right =
            composite_resource_id(&["org", "addon", "type", &format!("res{RESOURCE_ID_SEP}id")]);
        assert_ne!(left, right);
    }

    #[test]
    fn composite_is_injective_for_empty_and_split_segments() {
        // Empty segment vs. fewer segments, and a moved boundary, stay distinct.
        assert_ne!(
            composite_resource_id(&["", "a"]),
            composite_resource_id(&["a"])
        );
        assert_ne!(
            composite_resource_id(&["ab", "c"]),
            composite_resource_id(&["a", "bc"])
        );
    }

    #[test]
    fn acl_keys_for_different_orgs_do_not_collide() {
        let org_a = scoped_acl_resource_id("org-a", "contacts", "person", "p1");
        let org_b = scoped_acl_resource_id("org-b", "contacts", "person", "p1");
        assert_ne!(org_a, org_b);
        assert!(org_a.contains("org-a"));
        assert!(org_b.contains("org-b"));
    }

    #[test]
    fn explicit_share_keys_for_different_orgs_do_not_collide() {
        let org_a = scoped_explicit_share_resource_id(
            "org-a", "contacts", "person", "p1", "user", "7", "read",
        );
        let org_b = scoped_explicit_share_resource_id(
            "org-b", "contacts", "person", "p1", "user", "7", "read",
        );
        assert_ne!(org_a, org_b);
    }

    #[test]
    fn separator_prevents_segment_bleed() {
        // ("ab","c") and ("a","bc") must not produce the same key.
        let left = composite_resource_id(&["ab", "c"]);
        let right = composite_resource_id(&["a", "bc"]);
        assert_ne!(left, right);
    }
}
