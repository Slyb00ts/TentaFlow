// =============================================================================
// File: bus/instance.rs — BusInstanceId, the addon instance id of one
//       TentaBus instance (plan-app-platform §1.1). Fleet-identical
//       (`AddonManager::reconcile_synced_addon` mints the same
//       `{package_id}-{8hex}` on every node): a newtype, not a bare `String`,
//       because `org_id`, `topic` and `instance_id` are all strings and every
//       repository/gate signature below carries two or three of them —
//       swapping two positional `String` arguments compiles silently, a
//       `BusInstanceId` mismatch does not.
// =============================================================================

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use regex::Regex;

use super::BusServiceError;

/// The addon instance id of one TentaBus instance (`tentabus-<8hex>`).
///
/// `BusInstanceId::parse` WILL BE the trust boundary for every externally
/// supplied instance id (WS envelope, REST path, flow node config, SDK
/// input) — from W7, once `BusEnvelope.instance_id` is threaded through the
/// dispatcher and every call site parses through this type instead of a bare
/// `String`. Today (W2) the type validates its own construction, including
/// deserialization (`#[serde(try_from = "String")]` below — a plain derived
/// `Deserialize` would build the newtype straight from the wire string and
/// skip `parse` entirely), but nothing on the request path constructs one
/// yet. Validation here is SHAPE-only regardless of when it runs; existence,
/// enabled state and package membership are the app gate's job
/// (`dispatch::app_gate::require_instance_permission`), not this type's.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String")]
pub struct BusInstanceId(String);

impl BusInstanceId {
    /// `[addon].id` in `bus/app-manifest.toml` — the package every instance
    /// of this shape belongs to.
    pub const PACKAGE_ID: &'static str = "tentabus";

    /// Accepts only `tentabus-<8 lowercase hex>`, the real
    /// `unique_instance_id` shape (`addon/lifecycle.rs:478-487`). No
    /// test-only alternation: a production regex must not carry a fixture
    /// shape it does not need — tests mint real hex suffixes via
    /// `app_gate::test_support::install_app_instance`.
    pub fn parse(raw: &str) -> Result<Self, BusServiceError> {
        if !instance_id_regex().is_match(raw) {
            return Err(BusServiceError::InvalidArgument(format!(
                "invalid bus instance id '{raw}': expected 'tentabus-<8 lowercase hex>'"
            )));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn instance_id_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"^tentabus-[0-9a-f]{8}$").expect("bus instance id regex stays valid")
    })
}

impl fmt::Display for BusInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BusInstanceId {
    type Err = BusServiceError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl AsRef<str> for BusInstanceId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BusInstanceId {
    type Error = BusServiceError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl From<BusInstanceId> for String {
    fn from(id: BusInstanceId) -> Self {
        id.0
    }
}

/// PLAN-APP-PLATFORM W3->W4 bridge, NOT a real instance id anyone should
/// treat as production data.
///
/// W3 (this wave) makes `bus_topics`/`bus_partition_assignments`/
/// `bus_field_policies`/`bus_schema_subjects`/`bus_schema_versions`
/// instance-scoped end to end — repository, sync descriptors, materializer,
/// per-topic ACLs. But the ENGINE that would let a caller resolve "which
/// TentaBus instance is this request for" is still process-global; that
/// per-instance engine registry is W4's job (plan-app-platform §2's own W3
/// entry names this explicitly: "engine registry keyed by instance stays
/// out of scope"). Every caller in `bus::topics`, `bus::field_policies`,
/// `bus::schema_registry::registry`, `bus::replication::assignment`,
/// `dispatch::bus` and `services::bus_authorizer` that predates that
/// registry uses THIS constant instead of threading a real
/// `BusInstanceId` through its own signature — a placeholder chosen over
/// silently reusing some other id (like a hardcoded package id) precisely
/// because it is impossible to mistake for a real, minted instance id (see
/// `BusInstanceId::parse`'s regex: `tentabus-00000000` is shape-valid but
/// `AddonManager::reconcile_synced_addon` never mints an all-zero suffix).
///
/// W4 MUST delete this constant and thread the real per-request/per-engine
/// `BusInstanceId` through every one of the files named above — grep this
/// constant's name to find every call site that needs the real id.
pub const LEGACY_SINGLE_INSTANCE: &str = "tentabus-00000000";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_the_real_instance_shape() {
        let id = BusInstanceId::parse("tentabus-a1b2c3d4").expect("valid instance id");
        assert_eq!(id.as_str(), "tentabus-a1b2c3d4");
        assert_eq!(id.to_string(), "tentabus-a1b2c3d4");
    }

    #[test]
    fn parse_rejects_foreign_packages_and_malformed_suffixes() {
        assert!(
            BusInstanceId::parse("tentanas-a1b2c3d4").is_err(),
            "wrong package"
        );
        assert!(
            BusInstanceId::parse("tentabus-A1B2C3D4").is_err(),
            "uppercase hex"
        );
        assert!(
            BusInstanceId::parse("tentabus-a1b2c3d").is_err(),
            "7 hex digits"
        );
        assert!(
            BusInstanceId::parse("tentabus-a1b2c3d4e").is_err(),
            "9 hex digits"
        );
        assert!(BusInstanceId::parse("tentabus-").is_err(), "empty suffix");
        assert!(
            BusInstanceId::parse("tentabus").is_err(),
            "no suffix at all"
        );
        assert!(BusInstanceId::parse("").is_err(), "empty string");
        assert!(
            BusInstanceId::parse("tentabus-testinst").is_err(),
            "the old test-fixture alternation is gone from the production regex"
        );
    }

    #[test]
    fn from_str_matches_parse() {
        let id: BusInstanceId = "tentabus-deadbeef".parse().expect("valid instance id");
        assert_eq!(id.as_str(), "tentabus-deadbeef");
        assert!("tentabus-zzzzzzzz".parse::<BusInstanceId>().is_err());
    }

    #[test]
    fn round_trips_through_string_conversions() {
        let id = BusInstanceId::parse("tentabus-00000001").unwrap();
        let as_ref: &str = id.as_ref();
        assert_eq!(as_ref, "tentabus-00000001");
        let owned: String = id.clone().into();
        assert_eq!(owned, "tentabus-00000001");
        let back = BusInstanceId::try_from(owned).expect("round trip");
        assert_eq!(back, id);
    }

    #[test]
    fn serializes_as_a_plain_json_string() {
        let id = BusInstanceId::parse("tentabus-00000001").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"tentabus-00000001\"");
        let back: BusInstanceId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    /// The trust boundary claim only holds if deserialization actually runs
    /// `parse`. Without `#[serde(try_from = "String")]` serde would build the
    /// newtype straight from the wire string and this would deserialize fine.
    #[test]
    fn deserialize_rejects_a_malformed_id_instead_of_bypassing_parse() {
        assert!(
            serde_json::from_str::<BusInstanceId>("\"../../etc\"").is_err(),
            "deserialize must reject a shape parse() would reject"
        );
        assert!(
            serde_json::from_str::<BusInstanceId>("\"tentabus-testinst\"").is_err(),
            "deserialize must reject the retired test-fixture shape too"
        );
    }
}
