// =============================================================================
// File: provider_account.rs
// Purpose: Binary CBOR protocol for agent provider accounts — the identity a
//          CLI agent (Claude Code, Codex, Grok Build, Muse Code) runs as, and
//          the per-node runtime that can host it. One family for the admin
//          screens A01–A04, the node matrix N01 and the user's own list U01.
//
//          An account is NOT a `services` row: it lives in `provider_accounts`
//          and travels through the Sync Ledger, so the same account is usable
//          from every node of the org. That is why nothing here names a node
//          except where a node is the SUBJECT of the request (login, install,
//          runtime state) — the account itself has no node.
//
//          Core resolves every human-readable name (`owner_display_name`,
//          `node_name`, `user_display_name`, `agent_name`, grant
//          `display_name`) the same way `analytics.js` expects: the dashboard
//          never renders a bare UUID as a title.
//
//          Append-only, and a rename is the one change that breaks every
//          deployed peer while the round-trip tests stay green — ciborium tags
//          by NAME. A field added later MUST carry `#[serde(default)]`, or a
//          peer that omits it stops decoding.
// Example: MessageBody::ProviderAccountBody(
//              ProviderAccountPayload::AccountListRequest {
//                  engine_id: None, scope: None, query: None,
//              })
// =============================================================================

use serde::{Deserialize, Serialize};

// =============================================================================
// Shared structs
// =============================================================================

/// One account as the admin list (A01) and the detail window (A03) read it.
///
/// `credential_revision` is 0 when no credential has ever been stored — that,
/// not `status`, is what tells the UI whether "Zaloguj" is a first login or a
/// re-login. The material itself never appears here in any shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderAccountInfo {
    pub account_id: String,
    pub engine_id: String,
    pub display_name: String,
    /// 'global' | 'user'.
    pub scope: String,
    pub owner_user_id: Option<String>,
    pub owner_display_name: Option<String>,
    /// 'api_key' | 'provider_login'.
    pub credential_kind: String,
    /// The provider's own identity for this account (an e-mail, an account id),
    /// learned at the first successful login and immutable afterwards.
    pub provider_subject: Option<String>,
    pub plan_label: Option<String>,
    /// 'pending' | 'active' | 'needs_login' | 'disabled'.
    pub status: String,
    pub home_node_id: Option<String>,
    pub home_node_name: Option<String>,
    /// The node that WAS this account's home and has since been deleted from the
    /// registry, which is why `home_node_id` is `None` (migration 164).
    ///
    /// It is what tells A03's two no-home states apart. An account that was never
    /// homed has a complete session list on the answering node; one whose home
    /// was deleted may still be running its sessions on that machine — it left
    /// the registry over expired trust, not because it stopped — so the empty
    /// session cell must say "no data" instead of "Brak aktywnych sesji.".
    ///
    /// There is deliberately no `home_lost_node_name`: no node can resolve a
    /// display name for a node that is gone, and the client shortens the id the
    /// same way it shortens every other id Core cannot name.
    #[serde(default)]
    pub home_lost_node_id: Option<String>,
    pub credential_revision: i64,
    pub expires_at: Option<String>,
    pub grant_count: u32,
    pub session_count: u32,
    pub agent_count: u32,
    pub updated_at: String,
    /// Nodes that have materialized this account's credential AS MEASURED BY
    /// THE ANSWERING NODE — A01's "Używane na".
    ///
    /// `provider_account_node_state` is node-local and stays out of the ledger
    /// (it is a measurement, not a decision), so this is what the answering node
    /// knows and never the whole fleet: a peer that materialized the account and
    /// never told anybody is absent from it. Empty therefore means "not here and
    /// nothing reported", not "nowhere".
    #[serde(default)]
    pub used_on: Vec<AccountUsageNode>,
    /// How many turns one account may run at the same time on one node. 0 —
    /// what an account has until somebody sets it — means no limit, which is
    /// exactly how accounts behaved before this field existed.
    ///
    /// It is an account-wide decision and replicates with the account; the
    /// sessions it is compared against do not, so each node counts its own.
    #[serde(default)]
    pub max_sessions: i64,
}

/// One row of the Dostęp tab (A04). `subject_type` is 'user' | 'group' | 'org';
/// `subject_id` is empty for 'org', which is the whole organisation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GrantEntry {
    pub subject_type: String,
    pub subject_id: String,
    pub display_name: String,
    /// How many people a group grant covers; `None` for a user or org subject.
    pub member_count: Option<u32>,
    /// Who granted it, resolved to a name — A04's "Nadał". `None` when the
    /// grant names an actor this node cannot resolve (a peer's administrator
    /// who never replicated here); the raw id is deliberately not sent in its
    /// place, because the column would then read as a person.
    #[serde(default)]
    pub granted_by_name: Option<String>,
    /// When the grant was made. Empty on a request, which is a full replace and
    /// carries no history — the server stamps it.
    #[serde(default)]
    pub granted_at: String,
}

/// One live session on an account (A03). Runtime state, never replicated — a
/// remote account's sessions are read from the node that holds them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccountSessionInfo {
    pub session_id: String,
    pub user_id: String,
    pub user_display_name: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_name: Option<String>,
    pub node_id: String,
    pub node_name: String,
    pub started_at: String,
    pub last_used_at: Option<String>,
}

/// How one node stands with respect to one account (A03, "Na nodach").
/// `applied_revision` below the account's `credential_revision` means the node
/// has not materialized the current credential yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccountNodeInfo {
    pub node_id: String,
    pub node_name: String,
    pub receives_accounts: bool,
    pub applied_revision: i64,
    /// 'absent' | 'materializing' | 'ready' | 'error'.
    pub runtime_state: String,
    pub last_error: Option<String>,
}

/// An agent bound to this account (A03, "Agenci"). `bind_mode` is 'global'
/// (the agent names this account) or 'user' (the agent resolves the running
/// user's own account, and this one is theirs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccountAgentInfo {
    pub agent_id: String,
    pub agent_name: String,
    pub bind_mode: String,
}

/// One card of "Moje konta agentów" (U01). `can_delete` is false for a global
/// account the user only has a grant on: using it is not owning it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MyAccountInfo {
    pub account_id: String,
    pub engine_id: String,
    pub display_name: String,
    pub scope: String,
    pub status: String,
    pub provider_subject: Option<String>,
    pub plan_label: Option<String>,
    pub can_login: bool,
    pub can_delete: bool,
    pub last_used_at: Option<String>,
    /// 'api_key' | 'provider_login' — U01 offers "Zaloguj" for one and "Wklej
    /// klucz" for the other, and `can_login` alone cannot tell them apart.
    #[serde(default)]
    pub credential_kind: String,
    /// Nodes this account is materialized on, same meaning and the same limit
    /// as `ProviderAccountInfo::used_on`: what the answering node measured.
    #[serde(default)]
    pub used_on: Vec<AccountUsageNode>,
    /// How many sessions the ANSWERING NODE has open on the account.
    /// `provider_account_sessions` is runtime state and is not replicated, so a
    /// session running on a peer is not in this number.
    #[serde(default)]
    pub session_count: u32,
}

/// One row of the node matrix (N01): what the node is, whether it may hold
/// credentials, and which CLIs are installed on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RuntimeNodeInfo {
    pub node_id: String,
    pub node_name: String,
    pub online: bool,
    /// Whether the node can isolate a CLI process at all. A node that cannot
    /// run agents is shown, and shown as unusable, instead of being hidden.
    /// `None` is "not measured here": only the node itself can probe its own
    /// sandbox, so a remote node answers this from its own runtime report and
    /// stays unknown until it does — the same contract `WorkspaceNodeInfo`
    /// already uses for `supports_process_sandbox`.
    pub sandbox_capable: Option<bool>,
    pub receives_accounts: bool,
    pub engines: Vec<RuntimeEngineInfo>,
    /// How many accounts this node holds a credential for, as counted by the
    /// ANSWERING node from its own `provider_account_node_state` — which is
    /// node-local and not replicated, so a remote row reads 0 until that node
    /// answers for itself.
    pub account_count: u32,
    /// Operating system of the node ('linux' / 'macos' / …), N01's sub-line.
    /// `None` for a node that has not reported one, for the same reason
    /// `sandbox_capable` stays unknown: only a node knows what it runs, and
    /// nothing replicates it — a peer stays unknown until the matrix is read on
    /// that peer.
    #[serde(default)]
    pub os: Option<String>,
    /// True on the row of the node that ANSWERED, false on every peer row.
    ///
    /// Nothing but the answering node can make this claim: a node id the
    /// dashboard would otherwise compare against its own is not in the payload,
    /// so the window cannot tell which row is itself. Same contract as
    /// `WorkspaceNodeInfo::is_local`, and the same reason the Code Studio node
    /// picker already carries one.
    #[serde(default)]
    pub is_local: bool,
}

/// One cell of the node matrix: the state of one engine on one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RuntimeEngineInfo {
    pub engine_id: String,
    /// 'absent' | 'installing' | 'installed' | 'error'.
    pub install_state: String,
    pub version: Option<String>,
    pub last_error: Option<String>,
}

/// One engine of the catalog, so the "Dodaj konto" dialog can offer only the
/// credential kinds an engine actually supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EngineSummary {
    pub engine_id: String,
    pub display_name: String,
    pub supports_login: bool,
    pub supports_api_key: bool,
}

/// One node an account is present on, named rather than identified: the list is
/// rendered inline in a table cell, where a UUID is noise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccountUsageNode {
    pub node_id: String,
    pub node_name: String,
}

// =============================================================================
// Payload
// =============================================================================

/// Every provider-account request and response. Order is part of the contract
/// (append-only, never insert or reorder), and no variant or field may be
/// renamed without updating the frontend and the pins below
/// (`provider_account_wire_enums_are_pinned`,
/// `provider_account_wire_struct_fields_are_pinned`,
/// `provider_account_wire_golden`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProviderAccountPayload {
    // ----- accounts (A01, A03) -----
    AccountListRequest {
        engine_id: Option<String>,
        scope: Option<String>,
        query: Option<String>,
    },
    AccountListResponse {
        accounts: Vec<ProviderAccountInfo>,
        engines: Vec<EngineSummary>,
    },
    AccountGetRequest {
        account_id: String,
    },
    AccountGetResponse {
        account: ProviderAccountInfo,
        grants: Vec<GrantEntry>,
        sessions: Vec<AccountSessionInfo>,
        nodes: Vec<AccountNodeInfo>,
        agents: Vec<AccountAgentInfo>,
    },
    /// `scope` is 'global' (admin) or 'user' (anybody, for themselves).
    /// `owner_user_id` is ignored for a global account and defaults to the
    /// caller for a user account — a user cannot mint an account for somebody
    /// else, and the handler, not this shape, is what enforces it.
    AccountCreateRequest {
        engine_id: String,
        display_name: String,
        scope: String,
        owner_user_id: Option<String>,
        credential_kind: String,
    },
    AccountCreateResponse {
        account: ProviderAccountInfo,
    },
    AccountUpdateRequest {
        account_id: String,
        display_name: Option<String>,
        status: Option<String>,
        /// `None` leaves the limit as it is; 0 removes it. Unlike the other
        /// fields, 0 is a value and not an absence, which is why turning a
        /// limit off is expressible without a second "clear" flag.
        #[serde(default)]
        max_sessions: Option<i64>,
    },
    AccountDeleteRequest {
        account_id: String,
    },

    // ----- credential (api_key accounts only; a provider login goes through
    // the Login* exchange below) -----
    CredentialSetRequest {
        account_id: String,
        material: String,
    },
    CredentialClearRequest {
        account_id: String,
    },

    // ----- login (A02) -----
    LoginStartRequest {
        account_id: String,
        node_id: Option<String>,
    },
    LoginStartResponse {
        login_id: String,
        node_id: String,
        verification_url: String,
        /// i18n key of the step's instruction; the text is never sent.
        instruction_key: String,
        expires_at: String,
    },
    LoginInputRequest {
        login_id: String,
        value: String,
    },
    LoginStatusRequest {
        login_id: String,
    },
    LoginStatusResponse {
        /// 'awaiting_open' | 'awaiting_input' | 'verifying' | 'succeeded' |
        /// 'failed'.
        state: String,
        provider_subject: Option<String>,
        plan_label: Option<String>,
        message_key: Option<String>,
    },
    LoginCancelRequest {
        login_id: String,
    },

    // ----- access (A04) -----
    /// A FULL REPLACE of the account's grants: what is not in the list is
    /// revoked. The server diffs it into per-row inserts and deletes, so a
    /// revocation replicates as a tombstone instead of a truncate.
    GrantsSetRequest {
        account_id: String,
        grants: Vec<GrantEntry>,
    },
    GrantsSetResponse {
        grants: Vec<GrantEntry>,
    },

    // ----- sessions (A03) -----
    SessionListRequest {
        account_id: String,
    },
    SessionListResponse {
        sessions: Vec<AccountSessionInfo>,
    },
    SessionRevokeRequest {
        account_id: String,
        session_id: String,
    },

    // ----- the caller's own accounts (U01) -----
    MyAccountListRequest {
        engine_id: Option<String>,
    },
    MyAccountListResponse {
        accounts: Vec<MyAccountInfo>,
    },

    // ----- node runtime matrix (N01) -----
    RuntimeListRequest {},
    RuntimeListResponse {
        nodes: Vec<RuntimeNodeInfo>,
    },
    RuntimeSetReceivesAccountsRequest {
        node_id: String,
        enabled: bool,
    },
    RuntimeInstallRequest {
        node_id: String,
        engine_id: String,
    },
    RuntimeUninstallRequest {
        node_id: String,
        engine_id: String,
    },
    RuntimeStatusResponse {
        node: RuntimeNodeInfo,
    },

    /// Generic acknowledgement for the writes that have nothing to return.
    /// `message_key` is an i18n key, never a sentence.
    AccountOpAck {
        account_id: Option<String>,
        ok: bool,
        message_key: Option<String>,
    },
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;
    use crate::wire_pin::{self, hex_bytes, name_digest};

    /// This very file, read at compile time. The byte goldens pin a few
    /// SHAPES; the declaration goldens pin the top-level `pub struct` and
    /// `pub enum` declarations of THIS file — names, order, field types and
    /// the serde attributes that rewrite either.
    const SOURCE: &str = include_str!("provider_account.rs");

    #[test]
    fn provider_account_source_is_parseable() {
        wire_pin::assert_parseable(SOURCE);
    }

    #[test]
    fn provider_account_wire_enums_are_pinned() {
        let enums = wire_pin::wire_enums(SOURCE);
        let names: Vec<String> = enums.iter().map(|item| item.name.clone()).collect();
        assert_eq!(
            names,
            vec!["ProviderAccountPayload".to_string()],
            "wire enum SET or its order changed. A new enum needs its own row in the table \
             below before this test can pass."
        );

        // (enum, member count, digest of its attributes + variants)
        let pinned: &[(&str, usize, u64)] =
            &[("ProviderAccountPayload", 30, 0xc04d_9143_1f40_9d84)];
        assert_eq!(pinned.len(), enums.len());
        for (name, count, digest) in pinned {
            let item = enums
                .iter()
                .find(|item| &item.name == name)
                .unwrap_or_else(|| panic!("enum '{name}' is gone from the wire module"));
            let entries = item.entries();
            assert_eq!(
                item.members.len(),
                *count,
                "'{name}' variant COUNT changed. Appending is fine — update the count and the \
                 digest here in the same commit. Live entries:\n{}",
                entries.join("\n")
            );
            assert_eq!(
                name_digest(&entries),
                *digest,
                "'{name}' variant NAMES, their FIELDS, their ORDER or a serde attribute \
                 changed. ciborium tags variants by name and encodes their fields as a map, so \
                 any of those moves the wire while every round-trip test stays green. Live \
                 entries:\n{}",
                entries.join("\n")
            );
        }
    }

    #[test]
    fn provider_account_wire_struct_fields_are_pinned() {
        let structs = wire_pin::wire_structs(SOURCE);
        let names: Vec<String> = structs.iter().map(|item| item.name.clone()).collect();
        assert_eq!(
            names.len(),
            10,
            "wire struct COUNT changed. Live structs:\n{}",
            names.join("\n")
        );
        assert_eq!(
            name_digest(&names),
            0x06df_ec4c_dbdc_c900,
            "wire struct NAMES or their DECLARATION ORDER changed. Live structs:\n{}",
            names.join("\n")
        );

        // (struct, field count, digest of its attributes + "name: Type" fields)
        let pinned: &[(&str, usize, u64)] = &[
            ("ProviderAccountInfo", 21, 0x8c7a_396f_e84b_d8d3),
            ("GrantEntry", 6, 0x6c64_44d1_5f29_be89),
            ("AccountSessionInfo", 11, 0x3227_205e_b73c_ec8d),
            ("AccountNodeInfo", 6, 0x2d0a_7b9e_3aa3_acb6),
            ("AccountAgentInfo", 3, 0xf485_7274_2ec5_4bef),
            ("MyAccountInfo", 13, 0x8f3a_1ca1_fb6b_a66b),
            ("RuntimeNodeInfo", 9, 0x2d27_8aee_18b0_391a),
            ("RuntimeEngineInfo", 4, 0x3b72_3d70_7ebd_2541),
            ("EngineSummary", 4, 0x9d7c_c109_f7dd_c510),
            ("AccountUsageNode", 2, 0xa9b4_a56e_70f7_26dd),
        ];
        assert_eq!(pinned.len(), structs.len());
        for (name, count, digest) in pinned {
            let item = structs
                .iter()
                .find(|item| &item.name == name)
                .unwrap_or_else(|| panic!("struct '{name}' is gone from the wire module"));
            let entries = item.entries();
            assert_eq!(
                item.members.len(),
                *count,
                "'{name}' field COUNT changed. Adding a field with #[serde(default)] is the \
                 supported move — update the count and digest here. Live entries:\n{}",
                entries.join("\n")
            );
            assert_eq!(
                name_digest(&entries),
                *digest,
                "'{name}' field NAMES, TYPES, ORDER or a serde attribute changed. The dashboard \
                 decodes these by name and the wire encodes them by type, and a round-trip test \
                 cannot see any of it because it re-encodes with the new declaration. Live \
                 entries:\n{}",
                entries.join("\n")
            );
        }
    }

    /// Golden wire snapshot: ciborium encodes enum variants as a 1-element map
    /// keyed by the variant NAME (external tagging). Pinning exact bytes turns
    /// an accidental rename of a variant, a field or the
    /// `MessageBody::ProviderAccountBody` tag into a test failure.
    #[test]
    fn provider_account_wire_golden() {
        let req = ProviderAccountPayload::AccountGetRequest {
            account_id: "acc1".to_string(),
        };
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a1714163636f756e7447657452657175657374a16a6163636f756e745f69646461636331"),
            "AccountGetRequest wire drift"
        );

        let body = MessageBody::ProviderAccountBody(req.clone());
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17350726f76696465724163636f756e74426f6479a1714163636f756e74476574526571756\
                 57374a16a6163636f756e745f69646461636331"
            ),
            "MessageBody::ProviderAccountBody wire drift"
        );

        let decoded: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded, MessageBody::ProviderAccountBody(req));

        // An empty struct variant encodes as a map, NOT as the bare string a
        // unit variant would produce — that difference is the whole reason
        // `RuntimeListRequest` is declared `{}`.
        let runtime = ProviderAccountPayload::RuntimeListRequest {};
        assert_eq!(
            crate::cbor::encode(&runtime).expect("encode"),
            hex_bytes("a17252756e74696d654c69737452657175657374a0"),
            "RuntimeListRequest wire drift"
        );
    }

    /// `ProviderAccountInfo::max_sessions` is an account decision that
    /// replicates with the account, so it has to survive the wire — and it
    /// arrived after the struct did, so a payload that predates it has to
    /// decode to the documented "no limit" rather than fail. The field-count
    /// digest above cannot see either: it pins the declaration, not a value.
    #[test]
    fn the_account_session_limit_survives_the_wire_and_defaults_to_no_limit() {
        let account = ProviderAccountInfo {
            account_id: "acc1".to_string(),
            max_sessions: 4,
            ..Default::default()
        };
        let frame = MessageBody::ProviderAccountBody(ProviderAccountPayload::AccountListResponse {
            accounts: vec![account.clone()],
            engines: Vec::new(),
        });
        let decoded: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&frame).expect("encode")).expect("decode");
        let MessageBody::ProviderAccountBody(ProviderAccountPayload::AccountListResponse {
            accounts,
            ..
        }) = decoded
        else {
            panic!("the frame must decode as the response it was encoded from");
        };
        assert_eq!(
            accounts,
            vec![account.clone()],
            "the limit must survive a round trip"
        );
        assert_eq!(accounts[0].max_sessions, 4);

        // The same document with the field removed — what a peer built before
        // this field existed sends.
        let bytes = crate::cbor::encode(&account).expect("encode");
        let mut value: ciborium::value::Value = crate::cbor::decode(&bytes).expect("decode");
        let ciborium::value::Value::Map(entries) = &mut value else {
            panic!("a struct encodes as a map");
        };
        entries.retain(|(key, _)| key.as_text() != Some("max_sessions"));
        let without = crate::cbor::encode(&value).expect("encode");
        assert!(
            !without
                .windows(b"max_sessions".len())
                .any(|window| window == b"max_sessions"),
            "the fixture must really omit the field"
        );
        let decoded: ProviderAccountInfo = crate::cbor::decode(&without).expect("decode");
        assert_eq!(
            decoded.max_sessions, 0,
            "a payload without the field means the account has no limit, which is how accounts \
             behaved before the field existed"
        );
    }

    /// `home_lost_node_id` arrived after the struct did, so a payload from a peer
    /// that predates it has to decode — and decode to `None`, which is exactly
    /// what it means: a peer that never had the column has no account whose home
    /// it recorded as lost. If the default flipped, every account of an older
    /// peer would arrive claiming its home had been deleted.
    #[test]
    fn a_payload_without_the_lost_home_marker_defaults_to_no_loss() {
        let account = ProviderAccountInfo {
            account_id: "acc1".to_string(),
            home_lost_node_id: Some("gone-node".to_string()),
            ..Default::default()
        };
        let frame = MessageBody::ProviderAccountBody(ProviderAccountPayload::AccountListResponse {
            accounts: vec![account.clone()],
            engines: Vec::new(),
        });
        let decoded: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&frame).expect("encode")).expect("decode");
        let MessageBody::ProviderAccountBody(ProviderAccountPayload::AccountListResponse {
            accounts,
            ..
        }) = decoded
        else {
            panic!("the frame must decode as the response it was encoded from");
        };
        assert_eq!(
            accounts,
            vec![account.clone()],
            "the marker must survive a round trip"
        );
        assert_eq!(accounts[0].home_lost_node_id.as_deref(), Some("gone-node"));

        // The same struct with the field removed — what a peer built before the
        // marker existed sends.
        let bytes = crate::cbor::encode(&account).expect("encode");
        let mut value: ciborium::value::Value = crate::cbor::decode(&bytes).expect("decode");
        let ciborium::value::Value::Map(entries) = &mut value else {
            panic!("a struct encodes as a map");
        };
        entries.retain(|(key, _)| key.as_text() != Some("home_lost_node_id"));
        let without = crate::cbor::encode(&value).expect("encode");
        assert!(
            !without
                .windows(b"home_lost_node_id".len())
                .any(|window| window == b"home_lost_node_id"),
            "the fixture must really omit the field"
        );
        let decoded: ProviderAccountInfo = crate::cbor::decode(&without).expect("decode");
        assert_eq!(
            decoded.home_lost_node_id, None,
            "an account from a peer that does not carry the marker has not lost a home as far as \
             that peer can say"
        );
    }

    /// `RuntimeNodeInfo::is_local` arrived after the struct did, so a matrix
    /// encoded by a peer that predates it has to decode — and decode to FALSE.
    /// That default is what the UI reads as "not this machine": if it flipped,
    /// every remote row of an older peer's matrix would claim to be the node
    /// the operator is looking at, and A03 would label the wrong row.
    #[test]
    fn a_node_matrix_without_the_local_marker_defaults_every_row_to_remote() {
        let node = RuntimeNodeInfo {
            node_id: "peer".to_string(),
            is_local: true,
            ..Default::default()
        };
        let frame = MessageBody::ProviderAccountBody(ProviderAccountPayload::RuntimeListResponse {
            nodes: vec![node.clone()],
        });
        let decoded: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&frame).expect("encode")).expect("decode");
        let MessageBody::ProviderAccountBody(ProviderAccountPayload::RuntimeListResponse { nodes }) =
            decoded
        else {
            panic!("the frame must decode as the response it was encoded from");
        };
        assert_eq!(
            nodes,
            vec![node.clone()],
            "the marker must survive a round trip"
        );
        assert!(nodes[0].is_local);

        // The same struct with the field removed — what a peer built before the
        // marker existed sends.
        let bytes = crate::cbor::encode(&node).expect("encode");
        let mut value: ciborium::value::Value = crate::cbor::decode(&bytes).expect("decode");
        let ciborium::value::Value::Map(entries) = &mut value else {
            panic!("a struct encodes as a map");
        };
        entries.retain(|(key, _)| key.as_text() != Some("is_local"));
        let without = crate::cbor::encode(&value).expect("encode");
        assert!(
            !without
                .windows(b"is_local".len())
                .any(|window| window == b"is_local"),
            "the fixture must really omit the field"
        );
        let decoded: RuntimeNodeInfo = crate::cbor::decode(&without).expect("decode");
        assert!(
            !decoded.is_local,
            "a node matrix without the marker says nothing about which row is local, so no row \
             may claim it"
        );
    }
}
