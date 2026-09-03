// =============================================================================
// File: mesh/frame_policy.rs
// Purpose: Security gate classification for mesh frames before app dispatch.
//          Defines which frame types are allowed pre-trust (handshake) and
//          which require an established trust relationship.
// =============================================================================

use tentaflow_protocol::mesh::{
    MESH_MSG_HEARTBEAT, MESH_MSG_HELLO, MESH_MSG_KNOWN_PEERS, MESH_MSG_NODE_INFO,
    MESH_MSG_PAIRING_CONFIRM, MESH_MSG_PAIRING_REJECT, MESH_MSG_PAIRING_REQUEST,
    MESH_MSG_TOPOLOGY_ANNOUNCE,
};

/// Returns `true` for frames that may be accepted from peers that are NOT yet
/// trusted. Currently only the three pairing handshake frames qualify.
///
/// Every other mesh frame (heartbeat, hello, node_info, models,
/// commands, trusted-keys sync, trust revoked, ...) MUST come
/// from a peer that is already in the trusted set; otherwise it must be
/// dropped before any application-level state is touched.
#[inline]
pub fn is_pre_trust_frame(frame_type: u8) -> bool {
    matches!(
        frame_type,
        MESH_MSG_PAIRING_REQUEST | MESH_MSG_PAIRING_CONFIRM | MESH_MSG_PAIRING_REJECT
    )
}

/// Ramki, ktore NIEZAUFANY peer wysyla sam z siebie, w kolko, bez naszego
/// udzialu: heartbeat i rozglaszanie obecnosci. Odrzucamy je tak samo jak
/// reszte, ale NIE zapisujemy audytu — kazde odrzucenie to byl jeden INSERT,
/// a jeden niesparowany wezel w LAN-ie potrafil urosnac `audit_log` do 1,1 mln
/// wierszy (98,5% tabeli) i baze do 6,3 GB. To nie jest sygnal bezpieczenstwa,
/// tylko szum discovery: wezel, ktorego nie sparowano, nadal sie rozglasza.
///
/// Proba wyslania czegokolwiek INNEGO przez niezaufanego peera (komenda,
/// forward, sync) to juz sygnal i ta zostaje audytowana.
pub fn is_discovery_noise_frame(frame_type: u8) -> bool {
    matches!(
        frame_type,
        MESH_MSG_HEARTBEAT | MESH_MSG_HELLO | MESH_MSG_NODE_INFO | MESH_MSG_KNOWN_PEERS
            | MESH_MSG_TOPOLOGY_ANNOUNCE
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::mesh::*;

    #[test]
    fn pairing_request_is_pre_trust() {
        assert!(is_pre_trust_frame(MESH_MSG_PAIRING_REQUEST));
    }

    #[test]
    fn pairing_confirm_is_pre_trust() {
        assert!(is_pre_trust_frame(MESH_MSG_PAIRING_CONFIRM));
    }

    #[test]
    fn pairing_reject_is_pre_trust() {
        assert!(is_pre_trust_frame(MESH_MSG_PAIRING_REJECT));
    }

    #[test]
    fn heartbeat_is_post_trust() {
        assert!(!is_pre_trust_frame(MESH_MSG_HEARTBEAT));
    }

    #[test]
    fn hello_is_post_trust() {
        assert!(!is_pre_trust_frame(MESH_MSG_HELLO));
    }

    #[test]
    fn node_info_is_post_trust() {
        assert!(!is_pre_trust_frame(MESH_MSG_NODE_INFO));
    }

    #[test]
    fn command_is_post_trust() {
        assert!(!is_pre_trust_frame(MESH_MSG_COMMAND));
    }

    #[test]
    fn trusted_keys_sync_is_post_trust() {
        assert!(!is_pre_trust_frame(MESH_MSG_TRUSTED_KEYS_SYNC));
    }

    #[test]
    fn trust_revoked_is_post_trust() {
        assert!(!is_pre_trust_frame(MESH_MSG_TRUST_REVOKED));
    }

    #[test]
    fn topology_announce_is_post_trust() {
        assert!(!is_pre_trust_frame(MESH_MSG_TOPOLOGY_ANNOUNCE));
    }

    #[test]
    fn known_peers_is_post_trust() {
        assert!(!is_pre_trust_frame(MESH_MSG_KNOWN_PEERS));
    }

    #[test]
    fn unknown_discriminant_is_post_trust() {
        assert!(!is_pre_trust_frame(0xFF));
        assert!(!is_pre_trust_frame(0x00));
    }

    #[test]
    fn test_frame_proxy_request_is_not_pre_trust() {
        // F1b P3.C — a frame proxy request must only be honored from a
        // trust-paired peer; otherwise any untrusted node on the network
        // could pull arbitrary frame_url-targeted frames out of us.
        assert!(!is_pre_trust_frame(MESH_MSG_FRAME_PROXY_REQUEST));
    }

    #[test]
    fn test_frame_proxy_response_is_not_pre_trust() {
        // F1b P3.C — responses too: accepting a forged response pre-trust
        // would let an attacker inject frame bytes into our pending-map.
        assert!(!is_pre_trust_frame(MESH_MSG_FRAME_PROXY_RESPONSE));
    }
}

#[cfg(test)]
mod noise_tests {
    use super::*;
    use tentaflow_protocol::mesh::*;

    #[test]
    fn heartbeat_from_untrusted_peer_is_noise_not_audit() {
        // The frame that produced 98.5% of a 1.1M-row audit table.
        assert!(is_discovery_noise_frame(MESH_MSG_HEARTBEAT));
        assert!(is_discovery_noise_frame(MESH_MSG_HELLO));
        assert!(is_discovery_noise_frame(MESH_MSG_NODE_INFO));
        assert!(is_discovery_noise_frame(MESH_MSG_KNOWN_PEERS));
        assert!(is_discovery_noise_frame(MESH_MSG_TOPOLOGY_ANNOUNCE));
    }

    #[test]
    fn a_command_from_an_untrusted_peer_is_still_audited() {
        // Discovery noise is a neighbour announcing itself; a command is someone
        // trying to make us act. Only the first may be dropped silently.
        assert!(!is_discovery_noise_frame(MESH_MSG_COMMAND));
        assert!(!is_discovery_noise_frame(MESH_MSG_PAIRING_REQUEST));
    }

    #[test]
    fn noise_frames_are_still_rejected_pre_trust() {
        // Not auditing them must not turn into accepting them.
        assert!(!is_pre_trust_frame(MESH_MSG_HEARTBEAT));
        assert!(!is_pre_trust_frame(MESH_MSG_HELLO));
    }
}
