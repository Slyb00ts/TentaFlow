// =============================================================================
// File: bus/field_policies.rs — TentaBus field-level access policies
// =============================================================================
// SUM/tentabus/POLITYKI-POL.md, decided with the owner via AskUserQuestion
// on 01.09.2026:
//   - write violation -> mode=reject: the WHOLE batch is rejected on any
//     disallowed or missing-required field, never stripped.
//   - read violation  -> hide: a restricted field is simply omitted from the
//     projected payload. F4 B7 (DECYZJE-2026-09-22.md, 23.09) adds two more
//     read actions for a field the rule shows: MASK (last 3 characters kept,
//     the rest `*`, a value shorter than 5 characters masked whole) and HASH
//     (`blake3::keyed_hash` under the organization's key, 256 bit hex) — JSON,
//     XML and HL7 v2 only, never a binary format.
//   - granularity     -> top-level JSON object keys only, v1 has no nested
//     dotted-path support (e.g. `patient.address.street`).
//   - enforcement     -> external AND internal: a policy applies to EVERY
//     publish/fetch/peek on a policy-bearing topic, not just callers
//     crossing the REST/WS/bridge/host-function boundary. See
//     `BusService::publish`/`ConsumerHandle::fetch`/`BusService::peek` for
//     the three call sites.
//   - payload format  -> pluggable per topic (`payload_format::PayloadFormat`,
//     SUM/tentabus/POLITYKI-POL-FORMATY.md). v1 shipped JSON-object-only
//     (covers FHIR R4, since its resources are JSON); F0 moves that JSON
//     logic behind `payload_format::PayloadFieldFormat` as the reference
//     implementation and routes by the topic's `content_type`, so later
//     phases (XML, HL7 v2, Avro, Protobuf, Thrift) add a format without
//     touching this file's enforcement logic. A payload that does not
//     parse as its topic's resolved format fails closed
//     (`BusServiceError::FieldPolicyPayloadMalformed`) rather than
//     silently bypassing the policy.
//
// A topic with no matching `bus_field_policies` row is entirely unaffected
// (opt-in feature) — `resolve` returning `None` is a plain indexed point
// lookup, same uncached shape as `RbacBusAuthorizer::topic_acl_allows`
// (not `BusService::topic_config_cache`: caching risks a stale "no policy"
// entry surviving after a policy is created, which isn't worth it here
// without a measured need). This keeps the overwhelmingly common no-policy
// case exactly as fast as before this feature — the JSON decode/validate
// cost this file adds is only ever paid by a topic that actually opted in.
// =============================================================================

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;

use crate::auth::actor::ActorKind;
pub use crate::db::repository::BusFieldPolicyExpect;
use crate::db::repository::{self, BusFieldPolicyWrite, DbBusFieldPolicy};
use crate::db::DbPool;

use super::dlq;
use super::field_hash_key::{FieldHashKey, FieldHashKeys};
use super::payload_format::{FieldReplacer, PayloadFormat, ReplaceAction, ValueShape};
use super::{topics, BusServiceError};

/// Sentinel `subject_id` for the topic-wide wildcard row. Chosen over
/// `NULL` specifically to avoid SQLite's NULL-uniqueness pitfall in the
/// table's composite `PRIMARY KEY` — two `subject_type='any'` rows for the
/// same `(org_id, topic, direction)` would otherwise both satisfy a
/// `NULL`-based uniqueness check.
pub const SUBJECT_ANY: &str = "*";

/// Whose rule `resolve` looks up. An actor is what a publish/fetch/peek
/// resolves for, with the kind its entry point authenticated
/// (`BusCallContext::actor_kind`, `FP-subject-type`,
/// SUM/tentabus/DECYZJE-2026-09-22.md); `Group` and `Any` exist for the
/// administrator's "how does … see it" preview, which asks about a group or
/// the topic-wide rule directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicySubject<'a> {
    Actor(ActorKind, &'a str),
    Group(&'a str),
    Any,
}

impl<'a> PolicySubject<'a> {
    /// Parses a wire `(subject_type, subject_id)` pair; `None` for a type no
    /// rule can name or an `any` subject whose id is not `SUBJECT_ANY`.
    pub fn parse(subject_type: &str, subject_id: &'a str) -> Option<Self> {
        match subject_type {
            "user" => Some(PolicySubject::Actor(ActorKind::User, subject_id)),
            "addon" => Some(PolicySubject::Actor(ActorKind::Addon, subject_id)),
            "group" => Some(PolicySubject::Group(subject_id)),
            "any" if subject_id == SUBJECT_ANY => Some(PolicySubject::Any),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Write,
    Read,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Write => "write",
            Direction::Read => "read",
        }
    }

    /// Inverse of `as_str` — for wire-facing callers (dispatch layer)
    /// decoding the `direction: String` field of `FieldPolicySetRequest`/
    /// `FieldPolicyDeleteRequest`.
    pub fn parse(s: &str) -> Option<Direction> {
        match s {
            "write" => Some(Direction::Write),
            "read" => Some(Direction::Read),
            _ => None,
        }
    }
}

/// Resolved, decoded policy for one `(org_id, topic, subject, direction)`.
#[derive(Debug, Clone)]
pub struct FieldPolicy {
    /// Every field the rule shows, as it is or masked or hashed.
    pub fields: BTreeSet<String>,
    pub required_fields: BTreeSet<String>,
    /// Shown fields whose value a read replaces by its mask. A subset of
    /// `fields`, disjoint from `hashed_fields`; empty for a write rule.
    pub masked_fields: BTreeSet<String>,
    /// Shown fields whose value a read replaces by its keyed hash.
    pub hashed_fields: BTreeSet<String>,
}

/// What a read rule does to one field (`FieldPolicy::action`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldAction {
    Show,
    Hide,
    Mask,
    Hash,
}

impl FieldAction {
    /// The spelling of `BusFieldActionWire::action` and `capabilities`.
    pub fn as_str(self) -> &'static str {
        match self {
            FieldAction::Show => "show",
            FieldAction::Hide => "hide",
            FieldAction::Mask => "mask",
            FieldAction::Hash => "hash",
        }
    }
}

impl FieldPolicy {
    pub fn action(&self, field: &str) -> FieldAction {
        if !self.fields.contains(field) {
            FieldAction::Hide
        } else if self.hashed_fields.contains(field) {
            FieldAction::Hash
        } else if self.masked_fields.contains(field) {
            FieldAction::Mask
        } else {
            FieldAction::Show
        }
    }

    /// The key a read through this rule hashes with: `None` when the rule
    /// hashes nothing (no key is looked up for it), and also when the node
    /// holds no usable key — `project_read` then hides the hashed fields. A
    /// read never creates the key (`FieldHashKeys::for_read`).
    pub fn hash_key(&self, keys: &FieldHashKeys, org_id: &str) -> Option<Arc<FieldHashKey>> {
        if self.hashed_fields.is_empty() {
            return None;
        }
        keys.for_read(org_id)
    }
}

fn decode_names(
    json: &str,
    column: &str,
    topic: &str,
) -> Result<BTreeSet<String>, BusServiceError> {
    serde_json::from_str(json).map_err(|e| {
        BusServiceError::Db(format!(
            "corrupt bus_field_policies.{column} for topic '{topic}': {e}"
        ))
    })
}

pub(crate) fn decode(row: DbBusFieldPolicy, topic: &str) -> Result<FieldPolicy, BusServiceError> {
    let mut fields = decode_names(&row.fields_json, "fields_json", topic)?;
    let required_fields = match row.required_fields_json {
        Some(json) => decode_names(&json, "required_fields_json", topic)?,
        None => BTreeSet::new(),
    };
    let hashed_fields = match row.hashed_fields_json {
        Some(json) => decode_names(&json, "hashed_fields_json", topic)?,
        None => BTreeSet::new(),
    };
    let mut masked_fields = match row.masked_fields_json {
        Some(json) => decode_names(&json, "masked_fields_json", topic)?,
        None => BTreeSet::new(),
    };
    // `set_policy` keeps the two disjoint; a stored row that is not (a peer's)
    // gets the stricter of the two, the hash.
    masked_fields.retain(|f| !hashed_fields.contains(f));
    // The stored `fields_json` names only the fields shown as they are.
    fields.extend(masked_fields.iter().cloned());
    fields.extend(hashed_fields.iter().cloned());
    Ok(FieldPolicy {
        fields,
        required_fields,
        masked_fields,
        hashed_fields,
    })
}

/// Resolves the effective policy for `(org_id, topic, direction)` for
/// `subject`. For an actor the caller decides its KIND (`FP-subject-type`,
/// SUM/tentabus/DECYZJE-2026-09-22.md) from context it already has
/// (`BusCallContext::actor_kind`) —
/// `BusCallContext.actor` itself carries no type tag.
///
/// Precedence, most to least specific:
///   1. the subject's own row (`'user'`, `'addon'` or `'group'`). No row
///      can name an API key (`set_policy` and the table's CHECK admit no
///      `'api_key'` subject), so a general key reading over the records REST
///      (package K) meets only step 3 — the topic-wide rule, never a user's
///      rule that happens to carry the same id;
///   2. for a `User` only, a `subject_type='group'` row for one of the
///      groups the user belongs to (`repository::get_user_groups` — the
///      platform's one source of group membership; groups are not a
///      meaningful concept for an addon or API key actor, so this step is
///      skipped for `Addon` and `ApiKey`, and a `Group` subject has already
///      been matched in step 1).
///      Group rows are checked in the same `(group.id)` ascending order
///      `get_user_groups` already returns; the first match wins if a user
///      belongs to more than one group with a row on this topic — arbitrary
///      but deterministic, same tie-break shape as everywhere else in this
///      file that has no ordering signal from the domain itself;
///   3. the `subject_type='any'` topic-wide wildcard row (the only step for
///      `PolicySubject::Any`). An API key that reaches no row here while the
///      topic has rules of this direction for other subjects is refused
///      (`BusServiceError::KeyNeedsTopicWideRule`), never let through.
///
/// `Ok(None)` means "unrestricted" — no matching row at any level — which is
/// also forced unconditionally for every OTHER `__`-prefixed reserved topic
/// (broker-internal topics, e.g. `__bus.metrics`, are never
/// subject-policy-bearing).
///
/// `__dlq.<source_topic>` is the one reserved-prefix exception (owner
/// decision, SUM/tentabus/DECYZJE-2026-09-22.md, `FP-reserved-bypass`): a DLQ
/// reader must see exactly what they would see reading the SOURCE topic
/// directly, so this resolves and returns the source topic's policy instead
/// of exempting the DLQ topic. `set_policy` still refuses to store a policy
/// row directly on `__dlq.<source_topic>` — there is nothing for such a row
/// to do, since a lookup on the DLQ topic is redirected here before it could
/// ever be consulted.
pub fn resolve(
    pool: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
    subject: PolicySubject<'_>,
    direction: Direction,
) -> Result<Option<FieldPolicy>, BusServiceError> {
    if let Some(source_topic) = topic.strip_prefix(dlq::DLQ_TOPIC_PREFIX) {
        // A DLQ topic is only ever named after a non-reserved source topic
        // (`dlq::dlq_topic_name`), so `source_topic` never itself starts
        // with `RESERVED_PREFIX` — no risk of looping back into this branch.
        return resolve(pool, instance_id, org_id, source_topic, subject, direction);
    }
    // Every OTHER reserved topic is broker-owned and carries no subject of
    // its own, so no policy applies to it. `set_policy` refuses to store one
    // on such a topic, so no newly created row can be silently shadowed
    // here; a row written before that guard shipped still is —
    // `list_policies` returns it and `delete_policy` (which deliberately
    // does not refuse a reserved topic) removes it.
    if topic.starts_with(topics::RESERVED_PREFIX) {
        return Ok(None);
    }
    let lookup = |subject_type: &str, subject_id: &str| {
        repository::bus_field_policy_get(
            pool,
            instance_id,
            org_id,
            topic,
            subject_type,
            subject_id,
            direction.as_str(),
        )
    };
    let own = match subject {
        PolicySubject::Actor(kind, id) => Some((kind.as_str(), id)),
        PolicySubject::Group(id) => Some(("group", id)),
        PolicySubject::Any => None,
    };
    if let Some((subject_type, subject_id)) = own {
        if let Some(row) = lookup(subject_type, subject_id)? {
            return decode(row, topic).map(Some);
        }
    }
    if let PolicySubject::Actor(ActorKind::User, user_id) = subject {
        let groups = repository::get_user_groups(pool, user_id)
            .map_err(|e| BusServiceError::Db(e.to_string()))?;
        for group in groups {
            if let Some(row) = lookup("group", &group.id)? {
                return decode(row, topic).map(Some);
            }
        }
    }
    if let Some(row) = lookup("any", SUBJECT_ANY)? {
        return decode(row, topic).map(Some);
    }
    // A key meets no rule but the topic-wide one. Rules written only for
    // chosen people, groups or addons say this topic's data is restricted;
    // reading or writing past them because no rule names a key would be the
    // one caller they were never meant to leave unrestricted — so the key is
    // refused until the topic-wide rule exists (fail closed).
    if let PolicySubject::Actor(ActorKind::ApiKey, _) = subject {
        let restricted =
            repository::bus_field_policy_list_for_topic(pool, instance_id, org_id, topic)?
                .iter()
                .any(|row| row.direction == direction.as_str());
        if restricted {
            return Err(BusServiceError::KeyNeedsTopicWideRule {
                topic: topic.to_string(),
                direction: direction.as_str(),
            });
        }
    }
    Ok(None)
}

/// Validates one record's payload against a resolved WRITE policy.
/// `mode=reject` (the owner's choice): the first violation found is
/// returned as-is so the caller can fail the whole batch atomically, never
/// silently stripping the offending fields. `format` is the topic's
/// resolved wire format (`payload_format::PayloadFormat::from_content_type`)
/// — the caller resolves it once per topic, not per record.
pub fn validate_write(
    policy: &FieldPolicy,
    format: PayloadFormat,
    payload: &[u8],
    topic: &str,
) -> Result<(), BusServiceError> {
    let codec = format.codec();
    let present =
        codec
            .list_fields(payload)
            .map_err(|_| BusServiceError::FieldPolicyPayloadMalformed {
                topic: topic.to_string(),
                format: format.as_str(),
            })?;
    let mut disallowed: Vec<String> = present
        .iter()
        .filter(|f| !policy.fields.contains(*f))
        .cloned()
        .collect();
    if !disallowed.is_empty() {
        disallowed.sort();
        return Err(BusServiceError::FieldNotAllowed {
            topic: topic.to_string(),
            fields: disallowed,
        });
    }
    let mut missing: Vec<String> = policy
        .required_fields
        .iter()
        .filter(|f| !present.contains(*f))
        .cloned()
        .collect();
    if !missing.is_empty() {
        missing.sort();
        return Err(BusServiceError::RequiredFieldMissing {
            topic: topic.to_string(),
            fields: missing,
        });
    }
    Ok(())
}

/// Visible characters a mask keeps at the end of a value.
const MASK_KEPT_TAIL: usize = 3;
/// A value shorter than this is masked whole.
const MASK_MIN_LEN_FOR_TAIL: usize = 5;
/// The mask of a value whose length says nothing worth keeping (JSON boolean,
/// object, array): constant, so it cannot tell `true` from `false`.
const MASK_OPAQUE: &str = "***";

/// The mask of a value: its last 3 characters visible, every other character
/// an asterisk — the length is kept — and a value of fewer than 5 characters
/// masked whole. Characters are Unicode scalar values (`char`), not bytes: a
/// 5-character value of 4-byte emoji keeps its last 3 emoji. A combining mark
/// or a joiner counts as its own character, so a tail can start on one.
pub fn mask_text(value: &str) -> String {
    let len = value.chars().count();
    if len < MASK_MIN_LEN_FOR_TAIL {
        return "*".repeat(len);
    }
    let tail_start = value
        .char_indices()
        .nth(len - MASK_KEPT_TAIL)
        .map_or(value.len(), |(index, _)| index);
    let mut masked = "*".repeat(len - MASK_KEPT_TAIL);
    masked.push_str(&value[tail_start..]);
    masked
}

/// A rule's masks and hashes as a codec asks for them.
struct PolicyReplacer<'a> {
    policy: &'a FieldPolicy,
    hash_key: Option<&'a FieldHashKey>,
}

impl FieldReplacer for PolicyReplacer<'_> {
    fn action(&self, field: &str) -> Option<ReplaceAction> {
        match self.policy.action(field) {
            FieldAction::Hash => Some(ReplaceAction::Hash),
            FieldAction::Mask => Some(ReplaceAction::Mask),
            FieldAction::Show | FieldAction::Hide => None,
        }
    }

    fn replace(&self, action: ReplaceAction, text: &str, shape: ValueShape) -> String {
        match (action, shape) {
            (ReplaceAction::Hash, _) => match self.hash_key {
                Some(key) => key.hash_hex(text.as_bytes()),
                // `project_read` never lets a hashed field through without its key.
                None => MASK_OPAQUE.to_string(),
            },
            (ReplaceAction::Mask, ValueShape::Text) => mask_text(text),
            (ReplaceAction::Mask, ValueShape::Opaque) => MASK_OPAQUE.to_string(),
        }
    }
}

/// Projects one record's payload through a resolved READ policy: a field the
/// rule does not show is absent, one it masks or hashes carries the mask or
/// the hash of its value, every other shown field is as published. The result
/// is in the SAME wire format. Fails CLOSED — any payload the format's codec
/// cannot parse becomes that format's empty projection (e.g. `{}` for JSON)
/// rather than being returned verbatim, since a policy that cannot parse the
/// payload cannot prove what is safe to show.
///
/// `hash_key` is `FieldPolicy::hash_key` of the same rule. A rule that hashes
/// without being given its key hides the fields it would hash.
pub fn project_read(
    policy: &FieldPolicy,
    format: PayloadFormat,
    payload: &Bytes,
    hash_key: Option<&FieldHashKey>,
) -> Bytes {
    let codec = format.codec();
    let projected = if policy.masked_fields.is_empty() && policy.hashed_fields.is_empty() {
        codec.project(payload, &policy.fields, None)
    } else if policy.hashed_fields.is_empty() || hash_key.is_some() {
        codec.project(
            payload,
            &policy.fields,
            Some(&PolicyReplacer { policy, hash_key }),
        )
    } else {
        let shown: BTreeSet<String> = policy
            .fields
            .difference(&policy.hashed_fields)
            .cloned()
            .collect();
        codec.project(
            payload,
            &shown,
            Some(&PolicyReplacer {
                policy,
                hash_key: None,
            }),
        )
    };
    match projected {
        Ok(bytes) => bytes,
        Err(_) => codec.empty_projection(),
    }
}

/// What a rule on a topic of binary messages is refused with. The dashboard
/// recognizes the sentence (`schemas.js` `busRefusalText`): reword both together.
pub const BINARY_TOPIC_NO_POLICY: &str =
    "field policies cannot apply to a topic whose messages are binary";

/// What a rule that masks or hashes is refused with when it is not a read
/// rule. The dashboard recognizes the sentence (`schemas.js` `busRefusalText`):
/// reword both together.
pub const MASK_HASH_READ_ONLY: &str = "masking and hashing apply to read rules only";

/// What a rule naming a masked or hashed field it does not show is refused with.
pub const MASK_HASH_NOT_SHOWN: &str = "masked_fields and hashed_fields must be subsets of fields";

/// What a rule masking and hashing the same field is refused with.
pub const MASK_HASH_OVERLAP: &str = "a field cannot be both masked and hashed";

/// Creates or replaces the policy row for `(org_id, topic, subject_type,
/// subject_id, direction)`. `subject_type` must be one of `"user"`,
/// `"group"`, `"addon"` or `"any"` (mirroring the table's own widened `CHECK`
/// constraint since v167/`FP-subject-type`, validated here first so the
/// caller gets a clean `InvalidArgument` instead of a raw SQL error);
/// `subject_type="any"` requires `subject_id == SUBJECT_ANY`. For `"group"`,
/// `subject_id` is a `user_groups.id` — not validated for existence here,
/// same as `"user"`'s `subject_id` is never checked against a real user:
/// this feature is opt-in and tolerates a row naming a subject that has not
/// been created (or has since been deleted) yet.
/// `required_fields` must be a subset of `fields` — a field cannot be
/// required if it is not even allowed. `masked_fields` and `hashed_fields`
/// are the fields of `fields` a READ rule shows masked or hashed: subsets of
/// `fields`, disjoint, and empty for a write rule. A binary topic, which takes
/// no rule at all, takes no mask or hash either. A `__`-prefixed reserved topic is
/// refused outright, since `resolve` never applies a policy to one.
/// `expect` is checked atomically with the write: a stored rule that is not
/// what it names is `FieldPolicyChanged` and nothing is written.
#[allow(clippy::too_many_arguments)]
pub fn set_policy(
    pool: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
    subject_type: &str,
    subject_id: &str,
    direction: Direction,
    fields: &BTreeSet<String>,
    required_fields: &BTreeSet<String>,
    masked_fields: &BTreeSet<String>,
    hashed_fields: &BTreeSet<String>,
    expect: BusFieldPolicyExpect,
) -> Result<(), BusServiceError> {
    set_policy_marks(
        pool,
        instance_id,
        org_id,
        topic,
        subject_type,
        subject_id,
        direction,
        fields,
        required_fields,
        Some(masked_fields),
        Some(hashed_fields),
        expect,
    )
}

/// `set_policy` for a caller that may not know masks and hashes exist (a
/// client from before F4 B7 that sends only `fields`). `None` for
/// `masked_fields`/`hashed_fields` means "keep what the stored rule has,
/// narrowed to `fields`" — never "show them as they are": a stale editor must
/// not turn a masked field into a clear one. `Some` (even empty) is the new
/// list. Kept marks are written against the stored row's `updated_at_ms`
/// (`expect` is tightened to it when it was `Any`), so a rule changed meanwhile
/// is `FieldPolicyChanged` instead of being overwritten with stale marks.
#[allow(clippy::too_many_arguments)]
pub fn set_policy_marks(
    pool: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
    subject_type: &str,
    subject_id: &str,
    direction: Direction,
    fields: &BTreeSet<String>,
    required_fields: &BTreeSet<String>,
    masked_fields: Option<&BTreeSet<String>>,
    hashed_fields: Option<&BTreeSet<String>>,
    expect: BusFieldPolicyExpect,
) -> Result<(), BusServiceError> {
    let prepared = prepare_policy(
        pool,
        instance_id,
        org_id,
        topic,
        subject_type,
        subject_id,
        direction,
        fields,
        required_fields,
        masked_fields,
        hashed_fields,
        expect,
    )?;
    commit_policy(pool, prepared)
}

/// A rule that passed every check of `set_policy_marks` and waits to be
/// written (`commit_policy`). The step between the two is for a caller that
/// must do something only a VALID request deserves — the dispatcher makes the
/// organization's hash key here, so a request that is refused leaves no key
/// behind.
#[derive(Debug)]
pub struct PreparedPolicy {
    row: DbBusFieldPolicy,
    expect: BusFieldPolicyExpect,
    adds_hashed_fields: bool,
}

impl PreparedPolicy {
    /// Whether the rule hashes a field the stored rule did not: only then does
    /// it need the organization's key now. A rule that keeps the hashes it
    /// already has (an edit of something else) needs nothing the rule that
    /// introduced them did not.
    pub fn adds_hashed_fields(&self) -> bool {
        self.adds_hashed_fields
    }
}

/// Everything `set_policy_marks` checks, without writing.
#[allow(clippy::too_many_arguments)]
pub fn prepare_policy(
    pool: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
    subject_type: &str,
    subject_id: &str,
    direction: Direction,
    fields: &BTreeSet<String>,
    required_fields: &BTreeSet<String>,
    masked_fields: Option<&BTreeSet<String>>,
    hashed_fields: Option<&BTreeSet<String>>,
    expect: BusFieldPolicyExpect,
) -> Result<PreparedPolicy, BusServiceError> {
    let kept;
    let (masked_fields, hashed_fields, expect) = match (masked_fields, hashed_fields) {
        (Some(masked), Some(hashed)) => (masked, hashed, expect),
        (masked, hashed) => {
            let stored = repository::bus_field_policy_get(
                pool,
                instance_id,
                org_id,
                topic,
                subject_type,
                subject_id,
                direction.as_str(),
            )?;
            let keep = |column: &Option<String>, name: &str| -> Result<BTreeSet<String>, BusServiceError> {
                let names = match column {
                    Some(json) => decode_names(json, name, topic)?,
                    None => BTreeSet::new(),
                };
                Ok(names.intersection(fields).cloned().collect())
            };
            let stored_hashed = stored.as_ref().map(|r| &r.hashed_fields_json);
            let stored_masked = stored.as_ref().map(|r| &r.masked_fields_json);
            let hashed_kept = match hashed {
                Some(given) => given.clone(),
                None => keep(stored_hashed.unwrap_or(&None), "hashed_fields_json")?,
            };
            let mut masked_kept = match masked {
                Some(given) => given.clone(),
                None => keep(stored_masked.unwrap_or(&None), "masked_fields_json")?,
            };
            // A kept mark never overrides a given one: the stricter hash wins.
            if masked.is_none() {
                masked_kept.retain(|f| !hashed_kept.contains(f));
            }
            let expect = match (expect, &stored) {
                (BusFieldPolicyExpect::Any, Some(row)) => {
                    BusFieldPolicyExpect::UpdatedAt(row.updated_at_ms)
                }
                (BusFieldPolicyExpect::Any, None) => BusFieldPolicyExpect::Absent,
                (other, _) => other,
            };
            kept = (masked_kept, hashed_kept);
            (&kept.0, &kept.1, expect)
        }
    };
    if !["user", "group", "addon", "any"].contains(&subject_type) {
        return Err(BusServiceError::InvalidArgument(format!(
            "subject_type must be one of 'user', 'group', 'addon', 'any', got '{subject_type}'"
        )));
    }
    if subject_type == "any" && subject_id != SUBJECT_ANY {
        return Err(BusServiceError::InvalidArgument(format!(
            "subject_type='any' requires subject_id='{SUBJECT_ANY}'"
        )));
    }
    if !required_fields.is_subset(fields) {
        return Err(BusServiceError::InvalidArgument(
            "required_fields must be a subset of fields".to_string(),
        ));
    }
    if direction == Direction::Write && !(masked_fields.is_empty() && hashed_fields.is_empty()) {
        return Err(BusServiceError::InvalidArgument(
            MASK_HASH_READ_ONLY.to_string(),
        ));
    }
    if !masked_fields.is_subset(fields) || !hashed_fields.is_subset(fields) {
        return Err(BusServiceError::InvalidArgument(
            MASK_HASH_NOT_SHOWN.to_string(),
        ));
    }
    if !masked_fields.is_disjoint(hashed_fields) {
        return Err(BusServiceError::InvalidArgument(
            MASK_HASH_OVERLAP.to_string(),
        ));
    }
    // `resolve` forces "unrestricted" for every reserved topic, so a row
    // written here would be stored, listed back to the admin who created it,
    // and never enforced by anything. Refused for the same reason (and in the
    // same shape) as `topics::apply_schema_binding_guard`'s refusal to bind a
    // schema to a reserved topic.
    if topic.starts_with(topics::RESERVED_PREFIX) {
        return Err(BusServiceError::InvalidArgument(format!(
            "topic '{topic}' is a broker-internal reserved topic and cannot carry a field \
             policy: reserved topics are not covered by field-policy enforcement"
        )));
    }
    // SUM/tentabus/POLITYKI-POL-FORMATY.md (F0): a policy's field names are
    // meaningless without a wire format to interpret them against, and a
    // policy on a topic that does not exist is an orphaned row nothing can
    // ever enforce — both fail closed here rather than writing a silently
    // unusable row.
    let cfg = topics::get_topic(pool, instance_id, org_id, topic)?.ok_or_else(|| {
        BusServiceError::TopicNotFound {
            name: topic.to_string(),
        }
    })?;
    // A rule reads fields of JSON, XML or HL7 v2 text. A topic checked with an
    // Avro, Protobuf or Thrift pattern carries bytes of another shape: writes
    // would fail as malformed and reads would come back empty. (A topic with
    // the default `application/octet-stream` and no such pattern is the
    // pre-F0 JSON topic and stays allowed.)
    if let Some(subject) = cfg.schema_id.as_deref().filter(|s| !s.is_empty()) {
        let bound = repository::bus_schema_subject_get(pool, instance_id, org_id, subject)?;
        if let Some(kind) = bound
            .as_ref()
            .and_then(|row| super::schema_registry::SchemaType::parse(&row.schema_type))
            .filter(|kind| kind.is_binary())
        {
            return Err(BusServiceError::InvalidArgument(format!(
                "{BINARY_TOPIC_NO_POLICY}: topic '{topic}' is checked with the {} pattern '{subject}'",
                kind.as_str()
            )));
        }
    }
    let codec = PayloadFormat::from_content_type(&cfg.content_type).codec();
    for f in fields.iter().chain(required_fields.iter()) {
        codec.validate_field_name(f).map_err(|e| {
            BusServiceError::InvalidArgument(format!(
                "field '{f}' is not a valid field address for topic '{topic}'s payload format: {e}"
            ))
        })?;
    }
    let now = super::now_ms();
    let existing = repository::bus_field_policy_get(
        pool,
        instance_id,
        org_id,
        topic,
        subject_type,
        subject_id,
        direction.as_str(),
    )?;
    let created_at_ms = existing.as_ref().map(|r| r.created_at_ms).unwrap_or(now);
    let stored_hashed = match existing.as_ref().and_then(|r| r.hashed_fields_json.as_deref()) {
        Some(json) => decode_names(json, "hashed_fields_json", topic)?,
        None => BTreeSet::new(),
    };
    let adds_hashed_fields = !hashed_fields.is_subset(&stored_hashed);
    let row = DbBusFieldPolicy {
        instance_id: instance_id.to_string(),
        org_id: org_id.to_string(),
        topic: topic.to_string(),
        subject_type: subject_type.to_string(),
        subject_id: subject_id.to_string(),
        direction: direction.as_str().to_string(),
        // Only the fields shown as they are: see `DbBusFieldPolicy::fields_json`.
        fields_json: serde_json::to_string(
            &fields
                .iter()
                .filter(|f| !masked_fields.contains(*f) && !hashed_fields.contains(*f))
                .collect::<Vec<_>>(),
        )
        .map_err(|e| BusServiceError::Db(e.to_string()))?,
        required_fields_json: names_json(required_fields)?,
        created_at_ms,
        updated_at_ms: now,
        masked_fields_json: names_json(masked_fields)?,
        hashed_fields_json: names_json(hashed_fields)?,
    };
    Ok(PreparedPolicy {
        row,
        expect,
        adds_hashed_fields,
    })
}

/// Writes a rule `prepare_policy` accepted.
pub fn commit_policy(pool: &DbPool, prepared: PreparedPolicy) -> Result<(), BusServiceError> {
    let PreparedPolicy { row, expect, .. } = prepared;
    let topic = row.topic.as_str();
    // The topic and `expect` are checked again where the rule is written: the
    // topic may have been deleted, or the rule changed, since they were read.
    match repository::bus_field_policy_set(pool, &row, expect)? {
        BusFieldPolicyWrite::Written => Ok(()),
        BusFieldPolicyWrite::TopicMissing => Err(BusServiceError::TopicNotFound {
            name: topic.to_string(),
        }),
        BusFieldPolicyWrite::BinaryPattern {
            subject,
            schema_type,
        } => Err(BusServiceError::InvalidArgument(format!(
            "{BINARY_TOPIC_NO_POLICY}: topic '{topic}' is checked with the {schema_type} pattern '{subject}'"
        ))),
        BusFieldPolicyWrite::Changed => Err(BusServiceError::FieldPolicyChanged {
            topic: topic.to_string(),
        }),
    }
}

/// A set of field names as the JSON array a rule column stores, `None` for an
/// empty set.
fn names_json(names: &BTreeSet<String>) -> Result<Option<String>, BusServiceError> {
    if names.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(names)
        .map(Some)
        .map_err(|e| BusServiceError::Db(e.to_string()))
}

/// Removes a rule; with `expected_updated_at_ms` only while it still carries
/// that value (`FieldPolicyChanged` otherwise, nothing removed).
#[allow(clippy::too_many_arguments)]
pub fn delete_policy(
    pool: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
    subject_type: &str,
    subject_id: &str,
    direction: Direction,
    expected_updated_at_ms: Option<i64>,
) -> Result<(), BusServiceError> {
    let removed = repository::bus_field_policy_delete(
        pool,
        instance_id,
        org_id,
        topic,
        subject_type,
        subject_id,
        direction.as_str(),
        expected_updated_at_ms,
    )?;
    if removed {
        Ok(())
    } else {
        Err(BusServiceError::FieldPolicyChanged {
            topic: topic.to_string(),
        })
    }
}

pub fn list_policies(
    pool: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
) -> Result<Vec<DbBusFieldPolicy>, BusServiceError> {
    Ok(repository::bus_field_policy_list_for_topic(
        pool,
        instance_id,
        org_id,
        topic,
    )?)
}

#[cfg(test)]
mod tests {
    use tentaflow_protocol::environment::NodeEnvironment;

    use super::*;

    const TEST_INSTANCE: &str = "tentabus-00000001";
    const TEST_ORG: &str = "org-1";

    fn test_db() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("test db")
    }

    fn field_set(fields: &[&str]) -> BTreeSet<String> {
        fields.iter().map(|s| s.to_string()).collect()
    }

    fn policy(fields: &[&str], required: &[&str]) -> FieldPolicy {
        FieldPolicy {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            required_fields: required.iter().map(|s| s.to_string()).collect(),
            masked_fields: BTreeSet::new(),
            hashed_fields: BTreeSet::new(),
        }
    }

    #[test]
    fn validate_write_allows_payload_within_allow_list() {
        let p = policy(&["patient_id", "status"], &[]);
        let payload = br#"{"patient_id":"123","status":"ok"}"#;
        assert!(validate_write(&p, PayloadFormat::Json, payload, "t").is_ok());
    }

    #[test]
    fn validate_write_rejects_disallowed_field() {
        let p = policy(&["patient_id"], &[]);
        let payload = br#"{"patient_id":"123","ssn":"999-99-9999"}"#;
        let err = validate_write(&p, PayloadFormat::Json, payload, "t").unwrap_err();
        match err {
            BusServiceError::FieldNotAllowed { fields, .. } => {
                assert_eq!(fields, vec!["ssn".to_string()]);
            }
            other => panic!("expected FieldNotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn validate_write_rejects_missing_required_field() {
        let p = policy(&["patient_id", "status"], &["status"]);
        let payload = br#"{"patient_id":"123"}"#;
        let err = validate_write(&p, PayloadFormat::Json, payload, "t").unwrap_err();
        match err {
            BusServiceError::RequiredFieldMissing { fields, .. } => {
                assert_eq!(fields, vec!["status".to_string()]);
            }
            other => panic!("expected RequiredFieldMissing, got {other:?}"),
        }
    }

    #[test]
    fn validate_write_rejects_non_object_payload() {
        let p = policy(&["a"], &[]);
        let err = validate_write(&p, PayloadFormat::Json, b"[1,2,3]", "t").unwrap_err();
        assert!(matches!(
            err,
            BusServiceError::FieldPolicyPayloadMalformed { .. }
        ));
    }

    #[test]
    fn validate_write_rejects_malformed_json() {
        let p = policy(&["a"], &[]);
        let err = validate_write(&p, PayloadFormat::Json, b"not json", "t").unwrap_err();
        assert!(matches!(
            err,
            BusServiceError::FieldPolicyPayloadMalformed { .. }
        ));
    }

    #[test]
    fn project_read_filters_to_allowed_fields_only() {
        let p = policy(&["patient_id"], &[]);
        let payload = Bytes::from_static(br#"{"patient_id":"123","ssn":"999-99-9999"}"#);
        let out = project_read(&p, PayloadFormat::Json, &payload, None);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v, serde_json::json!({"patient_id": "123"}));
    }

    #[test]
    fn project_read_fails_closed_on_non_object_payload() {
        let p = policy(&["a"], &[]);
        let payload = Bytes::from_static(b"not json");
        let out = project_read(&p, PayloadFormat::Json, &payload, None);
        assert_eq!(out.as_ref(), b"{}");
    }

    #[test]
    fn project_read_fails_closed_on_top_level_array() {
        let p = policy(&["a"], &[]);
        let payload = Bytes::from_static(b"[1,2,3]");
        let out = project_read(&p, PayloadFormat::Json, &payload, None);
        assert_eq!(out.as_ref(), b"{}");
    }

    /// The reserved topic here EXISTS (created the way `ensure_dlq_topic`
    /// creates it), so every other check in `set_policy` passes: without the
    /// reserved-prefix guard the row is written, `list_policies` shows it to
    /// the admin, and `resolve` ignores it forever.
    #[test]
    fn set_policy_rejects_a_reserved_topic() {
        let db = test_db();
        let topic = "__dlq.orders.created";
        // A dead-letter topic exists only beside its source.
        topics::create_topic_bypassing_guards(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "orders.created",
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create source topic");
        topics::create_internal_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create internal DLQ topic");

        let err = set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            "any",
            SUBJECT_ANY,
            Direction::Read,
            &field_set(&["patient_id"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect_err("a reserved topic must not accept a field policy");
        match err {
            BusServiceError::InvalidArgument(msg) => {
                assert!(msg.contains(topic), "message must name the topic: {msg}");
                assert!(
                    msg.contains("reserved"),
                    "message must say the topic is reserved: {msg}"
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
        assert!(
            list_policies(&db, TEST_INSTANCE, TEST_ORG, topic)
                .expect("list policies")
                .is_empty(),
            "no row may be persisted for a reserved topic"
        );
    }

    /// Companion to the test above: the guard must reject reserved topics
    /// only, not every topic.
    #[test]
    fn set_policy_accepts_an_ordinary_topic() {
        let db = test_db();
        let topic = "orders.created";
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create user topic");

        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            "any",
            SUBJECT_ANY,
            Direction::Read,
            &field_set(&["patient_id"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("an ordinary topic still accepts a field policy");
        assert_eq!(
            list_policies(&db, TEST_INSTANCE, TEST_ORG, topic)
                .expect("list policies")
                .len(),
            1
        );
    }

    /// `FP-reserved-bypass` (SUM/tentabus/DECYZJE-2026-09-22.md): a DLQ
    /// reader must see exactly what they would see reading the source topic
    /// — resolving a read policy on `__dlq.<topic>` must return the SAME
    /// policy as resolving it on `<topic>` directly, not `None`.
    #[test]
    fn resolve_on_a_dlq_topic_inherits_the_source_topics_read_policy() {
        let db = test_db();
        let source = "orders.created";
        let cfg = topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            source,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create source topic")
        .0;
        let dlq_topic = dlq::dlq_topic_name(source);
        topics::create_internal_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            &dlq_topic,
            dlq::dlq_topic_options(&cfg),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create DLQ topic");

        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            source,
            "any",
            SUBJECT_ANY,
            Direction::Read,
            &field_set(&["patient_id"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("set a read policy on the source topic");

        let via_dlq = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            &dlq_topic,
            PolicySubject::Actor(ActorKind::User, "some-actor"),
            Direction::Read,
        )
        .expect("resolve on the DLQ topic")
        .expect("DLQ topic must inherit the source topic's read policy");
        assert_eq!(via_dlq.fields, field_set(&["patient_id"]));

        let via_source = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            source,
            PolicySubject::Actor(ActorKind::User, "some-actor"),
            Direction::Read,
        )
        .expect("resolve on the source topic")
        .expect("source topic has a policy");
        assert_eq!(via_dlq.fields, via_source.fields);
    }

    /// Companion: a DLQ topic whose source topic carries NO policy must
    /// still resolve to `None` (unrestricted), not fall through to some
    /// stale reserved-topic exemption path.
    #[test]
    fn resolve_on_a_dlq_topic_with_no_source_policy_is_unrestricted() {
        let db = test_db();
        let source = "orders.shipped";
        let cfg = topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            source,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create source topic")
        .0;
        let dlq_topic = dlq::dlq_topic_name(source);
        topics::create_internal_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            &dlq_topic,
            dlq::dlq_topic_options(&cfg),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create DLQ topic");

        let resolved = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            &dlq_topic,
            PolicySubject::Actor(ActorKind::User, "some-actor"),
            Direction::Read,
        )
        .expect("resolve on the DLQ topic");
        assert!(resolved.is_none());
    }

    /// A non-DLQ reserved topic (e.g. `__bus.metrics`) still gets the
    /// unconditional exemption — only `__dlq.*` inherits.
    #[test]
    fn resolve_on_a_non_dlq_reserved_topic_stays_exempt() {
        let db = test_db();
        let resolved = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "__bus.metrics",
            PolicySubject::Actor(ActorKind::User, "some-actor"),
            Direction::Read,
        )
        .expect("resolve on a broker-internal topic");
        assert!(resolved.is_none());
    }

    /// `FP-subject-type`: a `subject_type='addon'` row applies only to an
    /// `Addon`-kind actor, never to a `User`-kind actor with the same raw
    /// id string — the two subject kinds must not collide.
    #[test]
    fn resolve_matches_addon_subject_only_for_an_addon_actor() {
        let db = test_db();
        let topic = "orders.created";
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");

        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            "addon",
            "meeting-recorder",
            Direction::Read,
            &field_set(&["patient_id"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("set an addon-scoped read policy");

        let as_addon = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            PolicySubject::Actor(ActorKind::Addon, "meeting-recorder"),
            Direction::Read,
        )
        .expect("resolve as the addon actor")
        .expect("addon row must apply to the addon actor");
        assert_eq!(as_addon.fields, field_set(&["patient_id"]));

        let as_user = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            PolicySubject::Actor(ActorKind::User, "meeting-recorder"),
            Direction::Read,
        )
        .expect("resolve as a user actor sharing the same raw id string");
        assert!(
            as_user.is_none(),
            "an addon-scoped row must not leak onto a user actor with the same id"
        );
    }

    /// `FP-subject-type`: a `subject_type='group'` row applies to a `User`
    /// actor who belongs to that group, resolved through the platform's own
    /// group membership (`repository::get_user_groups`) — not to a user who
    /// does not belong.
    #[test]
    fn resolve_falls_through_to_a_group_policy_for_a_member() {
        let db = test_db();
        let topic = "orders.created";
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");

        let member_id = repository::create_user_account(
            &db,
            "group-member",
            "hash",
            "Group Member",
            "group-member@example.com",
        )
        .expect("create member user");
        let outsider_id = repository::create_user_account(
            &db,
            "outsider",
            "hash",
            "Outsider",
            "outsider@example.com",
        )
        .expect("create outsider user");
        let group_id =
            repository::create_group(&db, "triage-team", "reads triage fields").expect("group");
        repository::add_user_to_group(&db, &group_id, &member_id).expect("add member");

        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            "group",
            &group_id,
            Direction::Read,
            &field_set(&["patient_id"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("set a group-scoped read policy");

        let member_resolved = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            PolicySubject::Actor(ActorKind::User, &member_id),
            Direction::Read,
        )
        .expect("resolve for the group member")
        .expect("group row must apply to a member");
        assert_eq!(member_resolved.fields, field_set(&["patient_id"]));

        let outsider_resolved = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            PolicySubject::Actor(ActorKind::User, &outsider_id),
            Direction::Read,
        )
        .expect("resolve for a non-member");
        assert!(
            outsider_resolved.is_none(),
            "a group row must not apply to a user outside the group"
        );
    }

    /// Precedence: an exact `subject_type='user'` row for the actor wins
    /// over a `subject_type='group'` row for a group the same actor belongs
    /// to.
    #[test]
    fn resolve_prefers_an_exact_user_row_over_a_group_row() {
        let db = test_db();
        let topic = "orders.created";
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");

        let member_id = repository::create_user_account(
            &db,
            "specific-user",
            "hash",
            "Specific User",
            "specific-user@example.com",
        )
        .expect("create member user");
        let group_id = repository::create_group(&db, "triage-team", "group").expect("group");
        repository::add_user_to_group(&db, &group_id, &member_id).expect("add member");

        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            "group",
            &group_id,
            Direction::Read,
            &field_set(&["patient_id", "status"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("set a group-scoped read policy");
        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            "user",
            &member_id,
            Direction::Read,
            &field_set(&["patient_id"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("set a user-scoped read policy");

        let resolved = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            PolicySubject::Actor(ActorKind::User, &member_id),
            Direction::Read,
        )
        .expect("resolve for the user")
        .expect("a row applies");
        assert_eq!(
            resolved.fields,
            field_set(&["patient_id"]),
            "the exact user row must win over the group row"
        );
    }

    #[test]
    fn set_policy_refuses_a_topic_checked_with_a_binary_pattern() {
        use super::super::schema_registry::{registry, Compatibility, SchemaType};
        let db = test_db();
        for (kind, subject, text) in [
            (SchemaType::Thrift, "events.thrift", "struct X { 1: i32 a }"),
            (
                SchemaType::Avro,
                "events.avro",
                r#"{"type":"record","name":"X","fields":[]}"#,
            ),
            (
                SchemaType::Protobuf,
                "events.proto",
                "syntax = \"proto3\"; message X {}",
            ),
        ] {
            registry::register(
                &db,
                TEST_INSTANCE,
                TEST_ORG,
                subject,
                kind,
                text,
                Some(Compatibility::None),
                None,
            )
            .expect("register");
            let topic = format!("bytes.{}", kind.as_str());
            topics::create_topic(
                &db,
                TEST_INSTANCE,
                TEST_ORG,
                &topic,
                topics::TopicOptions {
                    content_type: Some("application/octet-stream".to_string()),
                    schema_id: Some(subject.to_string()),
                    ..Default::default()
                },
                NodeEnvironment::Prod,
                1_000,
            )
            .expect("create topic");
            let err = set_policy(
                &db,
                TEST_INSTANCE,
                TEST_ORG,
                &topic,
                "any",
                SUBJECT_ANY,
                Direction::Read,
                &field_set(&["a"]),
                &BTreeSet::new(),
                &std::collections::BTreeSet::new(),
                &std::collections::BTreeSet::new(),
                BusFieldPolicyExpect::Any,
            )
            .expect_err("a binary topic has no fields to hide");
            assert!(
                matches!(&err, BusServiceError::InvalidArgument(m)
                if m == &format!(
                    "field policies cannot apply to a topic whose messages are binary: topic '{topic}' is checked with the {} pattern '{subject}'",
                    kind.as_str()
                )),
                "{err:?}"
            );
            assert!(list_policies(&db, TEST_INSTANCE, TEST_ORG, &topic)
                .expect("list")
                .is_empty());
        }
    }

    #[test]
    fn a_binary_pattern_cannot_be_bound_under_existing_rules_but_can_once_they_are_gone() {
        use super::super::schema_registry::{registry, Compatibility, SchemaType};
        let db = test_db();
        registry::register(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "events.thrift",
            SchemaType::Thrift,
            "struct X { 1: i32 a }",
            Some(Compatibility::None),
            None,
        )
        .expect("register");
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "ruled.events",
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");
        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "ruled.events",
            "any",
            SUBJECT_ANY,
            Direction::Read,
            &field_set(&["a"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("a default topic takes a rule");
        let bind = || {
            topics::update_topic(
                &db,
                TEST_INSTANCE,
                TEST_ORG,
                "ruled.events",
                topics::TopicOptions {
                    schema_id: Some("events.thrift".to_string()),
                    ..Default::default()
                },
                2_000,
            )
        };
        let err = bind().expect_err("a binary pattern under existing rules");
        assert!(
            matches!(&err, BusServiceError::InvalidTopicConfig { reason }
                if reason == "field policies cannot apply to a topic whose messages are binary: cannot bind the thrift pattern 'events.thrift' to topic 'ruled.events' while field policies exist for it; delete its field policies first"),
            "{err:?}"
        );
        // A text pattern is no problem, and neither is the binary one once the rules are gone.
        delete_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "ruled.events",
            "any",
            SUBJECT_ANY,
            Direction::Read,
            None,
        )
        .expect("delete the rule");
        bind().expect("no rules left");
    }

    /// The two orders are also closed where the write happens: these tests go
    /// around the early checks of `set_policy` and `update_topic`, the way a
    /// request that lost a race would reach the repository.
    #[test]
    fn the_writer_itself_refuses_a_rule_on_a_topic_bound_to_a_binary_pattern() {
        use super::super::schema_registry::{registry, Compatibility, SchemaType};
        let db = test_db();
        registry::register(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "events.thrift",
            SchemaType::Thrift,
            "struct X { 1: i32 a }",
            Some(Compatibility::None),
            None,
        )
        .expect("register");
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "bytes.events",
            topics::TopicOptions {
                schema_id: Some("events.thrift".to_string()),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");
        let row = DbBusFieldPolicy {
            instance_id: TEST_INSTANCE.to_string(),
            org_id: TEST_ORG.to_string(),
            topic: "bytes.events".to_string(),
            subject_type: "any".to_string(),
            subject_id: SUBJECT_ANY.to_string(),
            direction: "read".to_string(),
            fields_json: "[\"a\"]".to_string(),
            required_fields_json: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            masked_fields_json: None,
            hashed_fields_json: None,
        };
        assert_eq!(
            repository::bus_field_policy_set(&db, &row, BusFieldPolicyExpect::Any).expect("write"),
            BusFieldPolicyWrite::BinaryPattern {
                subject: "events.thrift".to_string(),
                schema_type: "thrift".to_string()
            }
        );
        assert!(list_policies(&db, TEST_INSTANCE, TEST_ORG, "bytes.events")
            .expect("list")
            .is_empty());
    }

    #[test]
    fn the_writer_itself_refuses_a_binary_pattern_on_a_topic_with_rules_only_when_asked_to() {
        use super::super::schema_registry::{registry, Compatibility, SchemaType};
        let db = test_db();
        registry::register(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "events.thrift",
            SchemaType::Thrift,
            "struct X { 1: i32 a }",
            Some(Compatibility::None),
            None,
        )
        .expect("register");
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "ruled.events",
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");
        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "ruled.events",
            "any",
            SUBJECT_ANY,
            Direction::Read,
            &field_set(&["a"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("rule");
        let mut row = repository::bus_topic_get(&db, TEST_INSTANCE, TEST_ORG, "ruled.events")
            .expect("get")
            .expect("topic");
        row.schema_id = Some("events.thrift".to_string());
        assert_eq!(
            repository::bus_topic_update_checked(&db, &row, true).expect("update"),
            repository::BusTopicWrite::BinaryPatternUnderRules
        );
        let stored = repository::bus_topic_get(&db, TEST_INSTANCE, TEST_ORG, "ruled.events")
            .expect("get")
            .expect("topic");
        assert_eq!(stored.schema_id, None, "nothing was written");
        // An update that does not ask (a setting changed on an old topic) goes through.
        assert_eq!(
            repository::bus_topic_update_checked(&db, &row, false).expect("update"),
            repository::BusTopicWrite::Written
        );
    }

    #[test]
    fn set_policy_still_accepts_a_topic_with_a_text_pattern_and_the_default_content_type() {
        use super::super::schema_registry::{registry, Compatibility, SchemaType};
        let db = test_db();
        registry::register(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "events.json",
            SchemaType::JsonSchema,
            r#"{"type":"object"}"#,
            Some(Compatibility::None),
            None,
        )
        .expect("register");
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "json.events",
            topics::TopicOptions {
                content_type: Some("application/json".to_string()),
                schema_id: Some("events.json".to_string()),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");
        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "json.events",
            "any",
            SUBJECT_ANY,
            Direction::Read,
            &field_set(&["a"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect("a JSON pattern allows a rule");
    }

    #[test]
    fn set_policy_rejects_an_unknown_subject_type() {
        let db = test_db();
        let topic = "orders.created";
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");

        let err = set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            "robot",
            "some-id",
            Direction::Read,
            &field_set(&["patient_id"]),
            &BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            BusFieldPolicyExpect::Any,
        )
        .expect_err("an unknown subject_type must be rejected");
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));
    }

    /// The subjects the administrator's preview asks about directly: a
    /// group resolves its own row, else the topic-wide one — never a row of
    /// one of its members — and `Any` is the topic-wide row alone, even when
    /// the group has its own.
    #[test]
    fn resolve_for_a_group_or_any_subject_skips_member_rows() {
        let db = test_db();
        let topic = "orders.created";
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");
        let member_id =
            repository::create_user_account(&db, "g-member", "hash", "Member", "m@example.com")
                .expect("member");
        let group_id = repository::create_group(&db, "nurses", "").expect("group");
        let empty_group_id = repository::create_group(&db, "no-rule", "").expect("group");
        repository::add_user_to_group(&db, &group_id, &member_id).expect("add member");
        for (subject_type, subject_id, fields) in [
            ("any", SUBJECT_ANY, vec!["id"]),
            ("group", group_id.as_str(), vec!["id", "name"]),
            ("user", member_id.as_str(), vec!["id", "name", "pesel"]),
        ] {
            set_policy(
                &db,
                TEST_INSTANCE,
                TEST_ORG,
                topic,
                subject_type,
                subject_id,
                Direction::Read,
                &field_set(&fields),
                &BTreeSet::new(),
                &std::collections::BTreeSet::new(),
                &std::collections::BTreeSet::new(),
                BusFieldPolicyExpect::Any,
            )
            .expect("set policy");
        }
        let fields_of = |subject: PolicySubject<'_>| {
            resolve(
                &db,
                TEST_INSTANCE,
                TEST_ORG,
                topic,
                subject,
                Direction::Read,
            )
            .expect("resolve")
            .map(|p| p.fields)
        };
        assert_eq!(
            fields_of(PolicySubject::Group(&group_id)),
            Some(field_set(&["id", "name"]))
        );
        assert_eq!(
            fields_of(PolicySubject::Group(&empty_group_id)),
            Some(field_set(&["id"]))
        );
        assert_eq!(fields_of(PolicySubject::Any), Some(field_set(&["id"])));
        assert_eq!(
            fields_of(PolicySubject::Actor(ActorKind::User, &member_id)),
            Some(field_set(&["id", "name", "pesel"]))
        );
    }

    #[test]
    fn policy_subject_parse_accepts_the_four_kinds_only() {
        assert_eq!(
            PolicySubject::parse("user", "u-1"),
            Some(PolicySubject::Actor(ActorKind::User, "u-1"))
        );
        assert_eq!(
            PolicySubject::parse("addon", "a"),
            Some(PolicySubject::Actor(ActorKind::Addon, "a"))
        );
        assert_eq!(
            PolicySubject::parse("group", "g"),
            Some(PolicySubject::Group("g"))
        );
        assert_eq!(
            PolicySubject::parse("any", SUBJECT_ANY),
            Some(PolicySubject::Any)
        );
        assert_eq!(PolicySubject::parse("any", "u-1"), None);
        assert_eq!(PolicySubject::parse("api_key", "k"), None);
    }

    // ---- F4 B7: mask and hash ----------------------------------------------

    fn node_keys() -> FieldHashKeys {
        FieldHashKeys::new(
            test_db(),
            Arc::new(crate::crypto::SettingsCipher::new(&[5u8; 32])),
        )
    }

    fn rule(fields: &[&str], masked: &[&str], hashed: &[&str]) -> FieldPolicy {
        FieldPolicy {
            fields: field_set(fields),
            required_fields: BTreeSet::new(),
            masked_fields: field_set(masked),
            hashed_fields: field_set(hashed),
        }
    }

    fn create_topic_of(db: &DbPool, topic: &str, content_type: &str) {
        topics::create_topic(
            db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            topics::TopicOptions {
                content_type: Some(content_type.to_string()),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");
    }

    fn read_rule(
        db: &DbPool,
        topic: &str,
        fields: &[&str],
        masked: &[&str],
        hashed: &[&str],
    ) -> Result<(), BusServiceError> {
        set_policy(
            db,
            TEST_INSTANCE,
            TEST_ORG,
            topic,
            "any",
            SUBJECT_ANY,
            Direction::Read,
            &field_set(fields),
            &BTreeSet::new(),
            &field_set(masked),
            &field_set(hashed),
            BusFieldPolicyExpect::Any,
        )
    }

    #[test]
    fn the_mask_keeps_the_last_three_characters_and_hides_short_values_whole() {
        for (value, masked) in [
            ("", ""),
            ("a", "*"),
            ("ab", "**"),
            ("abc", "***"),
            ("abcd", "****"),
            ("abcde", "**cde"),
            ("abcdef", "***def"),
            ("4111111111111111", "*************111"),
        ] {
            assert_eq!(mask_text(value), masked, "{value:?}");
        }
    }

    #[test]
    fn the_mask_counts_unicode_characters_not_bytes() {
        // 4 characters, 8 bytes: masked whole.
        assert_eq!(mask_text("żółć"), "****");
        // 6 characters, 12 bytes: three kept, intact.
        assert_eq!(mask_text("zażółć"), "***ółć");
        // CJK: 3 bytes a character.
        assert_eq!(mask_text("山田太郎花子"), "***郎花子");
        // Emoji are 4 bytes; five of them keep the last three.
        assert_eq!(mask_text("😀😀😀😀😀"), "**😀😀😀");
        assert_eq!(mask_text("😀😀😀😀"), "****");
        // Combining marks and joiners count as characters of their own.
        assert_eq!(mask_text("e\u{301}e\u{301}x"), "**e\u{301}x");
    }

    #[test]
    fn four_kinds_of_field_in_one_json_payload() {
        let keys = node_keys();
        let policy = rule(&["id", "card", "name", "age"], &["card", "age"], &["name"]);
        keys.get_or_create(TEST_ORG).unwrap();
        let key = policy.hash_key(&keys, TEST_ORG);
        let payload = Bytes::from_static(
            br#"{"id":"7","ssn":"123-45-6789","card":"4111111111111111","name":"Jan","age":42}"#,
        );
        let out = project_read(&policy, PayloadFormat::Json, &payload, key.as_deref());
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let expected_hash = key.as_ref().unwrap().hash_hex(b"Jan");
        assert_eq!(
            value,
            serde_json::json!({
                "id": "7",
                "card": "*************111",
                "name": expected_hash,
                "age": "**"
            })
        );
        assert!(value.get("ssn").is_none());
    }

    #[test]
    fn four_kinds_of_field_in_one_xml_payload() {
        let keys = node_keys();
        let policy = rule(&["id", "card", "name"], &["card"], &["name"]);
        keys.get_or_create(TEST_ORG).unwrap();
        let key = policy.hash_key(&keys, TEST_ORG);
        let payload = Bytes::from_static(
            b"<P><id>7</id><ssn>123</ssn><card>4111111111111111</card><name>Jan</name></P>",
        );
        let out = project_read(&policy, PayloadFormat::Xml, &payload, key.as_deref());
        let hash = key.as_ref().unwrap().hash_hex(b"Jan");
        assert_eq!(
            String::from_utf8(out.to_vec()).unwrap(),
            format!("<P><id>7</id><card>*************111</card><name>{hash}</name></P>")
        );
    }

    #[test]
    fn four_kinds_of_field_in_one_hl7_message() {
        let keys = node_keys();
        let policy = rule(&["PID-1", "PID-3", "PID-5"], &["PID-5"], &["PID-3"]);
        keys.get_or_create(TEST_ORG).unwrap();
        let key = policy.hash_key(&keys, TEST_ORG);
        let message = Bytes::from_static(
            b"MSH|^~\\&|APP|FAC|RAPP|RFAC|20260902||ADT^A01|MSG1|P|2.5\rPID|1||MRN123||Kowalski^Jan||19800101|M\r",
        );
        let out = project_read(&policy, PayloadFormat::Hl7V2, &message, key.as_deref());
        let hash = key.as_ref().unwrap().hash_hex(b"MRN123");
        assert_eq!(
            String::from_utf8(out.to_vec()).unwrap(),
            // The name is masked as one text: `Kowalski^Jan` -> 9 asterisks and
            // the last three characters, whose `^` position is long gone.
            format!("MSH|^~\\&||||||||||\rPID|1||{hash}||*********Jan|||\r")
        );
    }

    #[test]
    fn a_policy_without_masks_or_hashes_never_touches_the_key() {
        let keys = node_keys();
        let policy = rule(&["a"], &[], &[]);
        assert!(policy.hash_key(&keys, TEST_ORG).is_none());
        // Not even a mask-only rule makes one.
        assert!(rule(&["a"], &["a"], &[])
            .hash_key(&keys, TEST_ORG)
            .is_none());
        // Nor does a rule that hashes: a read never creates the key.
        assert!(rule(&["a"], &[], &["a"])
            .hash_key(&keys, TEST_ORG)
            .is_none());
        assert!(keys.info(TEST_ORG).unwrap().is_none());
    }

    #[test]
    fn a_rule_that_hashes_without_its_key_hides_the_hashed_fields() {
        let policy = rule(&["a", "b", "c"], &["b"], &["c"]);
        let payload = Bytes::from_static(br#"{"a":"keep","b":"masked-value","c":"secret"}"#);
        let out = project_read(&policy, PayloadFormat::Json, &payload, None);
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value, serde_json::json!({"a": "keep", "b": "*********lue"}));
    }

    #[test]
    fn hashes_are_stable_per_organization_and_change_with_the_key() {
        let keys = node_keys();
        let policy = rule(&["name"], &[], &["name"]);
        let payload = Bytes::from_static(br#"{"name":"Jan"}"#);
        let hash_in = |org: &str| {
            keys.get_or_create(org).unwrap();
            let key = policy.hash_key(&keys, org);
            let out = project_read(&policy, PayloadFormat::Json, &payload, key.as_deref());
            serde_json::from_slice::<serde_json::Value>(&out).unwrap()["name"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let first = hash_in("org-1");
        assert_eq!(first, hash_in("org-1"), "deterministic");
        assert_ne!(first, hash_in("org-2"), "isolated between organizations");

        keys.rotate("org-1", 1).unwrap();
        assert_ne!(first, hash_in("org-1"), "a rotation changes the hash");
    }

    #[test]
    fn decode_joins_shown_masked_and_hashed_fields_and_prefers_the_hash() {
        let db = test_db();
        create_topic_of(&db, "p.json", "application/json");
        read_rule(&db, "p.json", &["a", "b", "c"], &["b"], &["c"]).unwrap();
        let row = repository::bus_field_policy_get(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "p.json",
            "any",
            SUBJECT_ANY,
            "read",
        )
        .unwrap()
        .unwrap();
        // A node that ignores the new columns sees only what is shown as it is.
        assert_eq!(row.fields_json, r#"["a"]"#);
        assert_eq!(row.masked_fields_json.as_deref(), Some(r#"["b"]"#));
        assert_eq!(row.hashed_fields_json.as_deref(), Some(r#"["c"]"#));

        let policy = decode(row.clone(), "p.json").unwrap();
        assert_eq!(policy.fields, field_set(&["a", "b", "c"]));
        assert_eq!(policy.action("a"), FieldAction::Show);
        assert_eq!(policy.action("b"), FieldAction::Mask);
        assert_eq!(policy.action("c"), FieldAction::Hash);
        assert_eq!(policy.action("zzz"), FieldAction::Hide);

        // A row from a peer that names a field as both gets the stricter one.
        let both = DbBusFieldPolicy {
            masked_fields_json: Some(r#"["b","c"]"#.to_string()),
            ..row.clone()
        };
        let policy = decode(both, "p.json").unwrap();
        assert_eq!(policy.action("c"), FieldAction::Hash);
        assert_eq!(policy.action("b"), FieldAction::Mask);

        // A row from before the migration masks and hashes nothing.
        let old = DbBusFieldPolicy {
            fields_json: r#"["a","b"]"#.to_string(),
            masked_fields_json: None,
            hashed_fields_json: None,
            ..row
        };
        let policy = decode(old, "p.json").unwrap();
        assert!(policy.masked_fields.is_empty() && policy.hashed_fields.is_empty());
        assert_eq!(policy.action("b"), FieldAction::Show);
    }

    #[test]
    fn a_rule_without_masks_is_stored_as_before() {
        let db = test_db();
        create_topic_of(&db, "p.json", "application/json");
        read_rule(&db, "p.json", &["a", "b"], &[], &[]).unwrap();
        let row = repository::bus_field_policy_get(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "p.json",
            "any",
            SUBJECT_ANY,
            "read",
        )
        .unwrap()
        .unwrap();
        assert_eq!(row.fields_json, r#"["a","b"]"#);
        assert_eq!(
            (row.masked_fields_json, row.hashed_fields_json),
            (None, None)
        );
    }

    #[test]
    fn set_policy_refuses_masks_that_are_not_a_read_rule_over_shown_fields() {
        let db = test_db();
        create_topic_of(&db, "p.json", "application/json");
        let refusal = |result: Result<(), BusServiceError>| match result {
            Err(BusServiceError::InvalidArgument(message)) => message,
            other => panic!("expected InvalidArgument, got {other:?}"),
        };
        // A mask on a field the rule does not show.
        assert_eq!(
            refusal(read_rule(&db, "p.json", &["a"], &["b"], &[])),
            MASK_HASH_NOT_SHOWN
        );
        assert_eq!(
            refusal(read_rule(&db, "p.json", &["a"], &[], &["b"])),
            MASK_HASH_NOT_SHOWN
        );
        // Masked and hashed at once.
        assert_eq!(
            refusal(read_rule(&db, "p.json", &["a"], &["a"], &["a"])),
            MASK_HASH_OVERLAP
        );
        // A write rule has no mask.
        assert_eq!(
            refusal(set_policy(
                &db,
                TEST_INSTANCE,
                TEST_ORG,
                "p.json",
                "any",
                SUBJECT_ANY,
                Direction::Write,
                &field_set(&["a"]),
                &BTreeSet::new(),
                &field_set(&["a"]),
                &BTreeSet::new(),
                BusFieldPolicyExpect::Any,
            )),
            MASK_HASH_READ_ONLY
        );
        assert!(list_policies(&db, TEST_INSTANCE, TEST_ORG, "p.json")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn set_policy_refuses_the_structural_msh_fields_as_masked_or_hashed() {
        let db = test_db();
        create_topic_of(&db, "adt.hl7", "application/hl7-v2");
        for structural in ["MSH-1", "MSH-2"] {
            for (masked, hashed) in [(&[structural][..], &[][..]), (&[][..], &[structural][..])] {
                let err = read_rule(&db, "adt.hl7", &[structural], masked, hashed)
                    .expect_err("MSH-1 and MSH-2 can never be replaced");
                assert!(
                    matches!(&err, BusServiceError::InvalidArgument(m) if m.contains(structural)),
                    "{err:?}"
                );
            }
        }
        read_rule(&db, "adt.hl7", &["PID-3", "PID-5"], &["PID-5"], &["PID-3"]).unwrap();
    }

    #[test]
    fn set_policy_refuses_masks_on_a_topic_checked_with_a_binary_pattern() {
        use super::super::schema_registry::{registry, Compatibility, SchemaType};
        let db = test_db();
        registry::register(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "events.avro",
            SchemaType::Avro,
            r#"{"type":"record","name":"X","fields":[]}"#,
            Some(Compatibility::None),
            None,
        )
        .expect("register");
        topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "bytes.avro",
            topics::TopicOptions {
                content_type: Some("application/octet-stream".to_string()),
                schema_id: Some("events.avro".to_string()),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create topic");
        for (masked, hashed) in [(&["a"][..], &[][..]), (&[][..], &["a"][..])] {
            let err = read_rule(&db, "bytes.avro", &["a"], masked, hashed)
                .expect_err("a binary topic takes no mask or hash");
            assert!(
                matches!(&err, BusServiceError::InvalidArgument(m)
                    if m.starts_with(BINARY_TOPIC_NO_POLICY)),
                "{err:?}"
            );
        }
        assert!(list_policies(&db, TEST_INSTANCE, TEST_ORG, "bytes.avro")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_dlq_topic_resolves_the_masks_and_hashes_of_its_source() {
        let db = test_db();
        let source = "orders.created";
        let cfg = topics::create_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            source,
            topics::TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create source topic")
        .0;
        let dlq_topic = dlq::dlq_topic_name(source);
        topics::create_internal_topic(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            &dlq_topic,
            dlq::dlq_topic_options(&cfg),
            NodeEnvironment::Prod,
            1_000,
        )
        .expect("create DLQ topic");
        read_rule(&db, source, &["id", "card", "name"], &["card"], &["name"]).unwrap();

        let resolved = resolve(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            &dlq_topic,
            PolicySubject::Actor(ActorKind::User, "reader"),
            Direction::Read,
        )
        .unwrap()
        .expect("the DLQ topic inherits the source's rule");
        assert_eq!(resolved.masked_fields, field_set(&["card"]));
        assert_eq!(resolved.hashed_fields, field_set(&["name"]));
        assert_eq!(resolved.fields, field_set(&["id", "card", "name"]));
    }

    #[test]
    fn hashing_usage_counts_the_read_rules_topics_and_instances_that_hash() {
        let db = test_db();
        for topic in ["a.json", "b.json", "c.json"] {
            create_topic_of(&db, topic, "application/json");
        }
        assert_eq!(
            repository::bus_field_hash_usage(&db, TEST_ORG).unwrap(),
            repository::BusFieldHashUsage::default()
        );
        read_rule(&db, "a.json", &["x", "y"], &[], &["x"]).unwrap();
        // A second subject on the same topic is a second rule, not a second topic.
        set_policy(
            &db,
            TEST_INSTANCE,
            TEST_ORG,
            "a.json",
            "user",
            "u-1",
            Direction::Read,
            &field_set(&["x"]),
            &BTreeSet::new(),
            &BTreeSet::new(),
            &field_set(&["x"]),
            BusFieldPolicyExpect::Any,
        )
        .unwrap();
        read_rule(&db, "b.json", &["x"], &["x"], &[]).unwrap(); // masks only
        read_rule(&db, "c.json", &["x"], &[], &["x"]).unwrap();
        assert_eq!(
            repository::bus_field_hash_usage(&db, TEST_ORG).unwrap(),
            repository::BusFieldHashUsage {
                rules: 3,
                topics: 2,
                instances: 1
            }
        );
        assert_eq!(
            repository::bus_field_hash_usage(&db, "org-other").unwrap(),
            repository::BusFieldHashUsage::default()
        );
    }
}
