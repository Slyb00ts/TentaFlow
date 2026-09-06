// =============================================================================
// Plik: sync/ledger/validation.rs
// Opis: Walidacja podpisow, integralnosci hash-chain i Merkle summary dla Sync Ledger.
// =============================================================================

use super::types::{
    hash_canonical, signing_bytes_for_hash, LedgerResult, OperationId, RedactedRecord,
    SyncLedgerError, SyncMerkleSummary, SyncOperation, SyncOperationSigner, SyncOperationVerifier,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::collections::BTreeMap;

const MERKLE_LEAF_DOMAIN: &[u8] = b"tentaflow-sync-merkle-leaf-v1";
const MERKLE_NODE_DOMAIN: &[u8] = b"tentaflow-sync-merkle-node-v1";

pub struct Ed25519OperationSigner {
    node_id: String,
    signing_key: SigningKey,
}

impl Ed25519OperationSigner {
    pub fn new(node_id: impl Into<String>, signing_key: SigningKey) -> LedgerResult<Self> {
        let node_id = node_id.into();
        if node_id.trim().is_empty() {
            return Err(SyncLedgerError::EmptyIdentifier("node_id"));
        }
        Ok(Self {
            node_id,
            signing_key,
        })
    }
}

impl SyncOperationSigner for Ed25519OperationSigner {
    fn node_id(&self) -> &str {
        &self.node_id
    }

    fn sign_operation(&self, message: &[u8]) -> LedgerResult<Vec<u8>> {
        Ok(self.signing_key.sign(message).to_bytes().to_vec())
    }
}

pub struct TrustedKeyOperationVerifier {
    keys: BTreeMap<String, VerifyingKey>,
}

impl TrustedKeyOperationVerifier {
    pub fn new(keys: BTreeMap<String, VerifyingKey>) -> Self {
        Self { keys }
    }

    pub fn from_hex_keys(keys: BTreeMap<String, String>) -> LedgerResult<Self> {
        let mut parsed = BTreeMap::new();
        for (node_id, key_hex) in keys {
            let key = parse_verifying_key(&node_id, &key_hex)?;
            parsed.insert(node_id, key);
        }
        Ok(Self::new(parsed))
    }

    pub fn single(node_id: impl Into<String>, verifying_key: VerifyingKey) -> LedgerResult<Self> {
        let node_id = node_id.into();
        if node_id.trim().is_empty() {
            return Err(SyncLedgerError::EmptyIdentifier("node_id"));
        }
        let mut keys = BTreeMap::new();
        keys.insert(node_id, verifying_key);
        Ok(Self { keys })
    }
}

impl SyncOperationVerifier for TrustedKeyOperationVerifier {
    fn verify_operation_signature(&self, operation: &SyncOperation) -> LedgerResult<()> {
        operation.validate_integrity()?;
        let actor_node_id = &operation.body.actor_node_id;
        let key =
            self.keys
                .get(actor_node_id)
                .ok_or_else(|| SyncLedgerError::InvalidPublicKey {
                    actor_node_id: actor_node_id.clone(),
                })?;
        verify_signature(
            actor_node_id,
            key,
            &operation.signing_bytes(),
            &operation.signature,
        )
    }

    fn verify_redacted_signature(&self, record: &RedactedRecord) -> LedgerResult<()> {
        if record.op_id != OperationId::from_hash(record.operation_hash) {
            return Err(SyncLedgerError::InvalidOperationId {
                expected: OperationId::from_hash(record.operation_hash),
                actual: record.op_id,
            });
        }
        let key = self.keys.get(&record.actor_node_id).ok_or_else(|| {
            SyncLedgerError::InvalidPublicKey {
                actor_node_id: record.actor_node_id.clone(),
            }
        })?;
        verify_signature(
            &record.actor_node_id,
            key,
            &signing_bytes_for_hash(record.operation_hash),
            &record.signature,
        )
    }
}

pub struct HexNodeIdOperationVerifier;

impl SyncOperationVerifier for HexNodeIdOperationVerifier {
    fn verify_operation_signature(&self, operation: &SyncOperation) -> LedgerResult<()> {
        operation.validate_integrity()?;
        let actor_node_id = &operation.body.actor_node_id;
        let key = parse_verifying_key(actor_node_id, actor_node_id)?;
        verify_signature(
            actor_node_id,
            &key,
            &operation.signing_bytes(),
            &operation.signature,
        )
    }

    fn verify_redacted_signature(&self, record: &RedactedRecord) -> LedgerResult<()> {
        // op_id MUST equal the carried operation_hash: the signature is over the
        // hash, so this binds the receiver's chain key to the signed identity.
        if record.op_id != OperationId::from_hash(record.operation_hash) {
            return Err(SyncLedgerError::InvalidOperationId {
                expected: OperationId::from_hash(record.operation_hash),
                actual: record.op_id,
            });
        }
        let key = parse_verifying_key(&record.actor_node_id, &record.actor_node_id)?;
        verify_signature(
            &record.actor_node_id,
            &key,
            &signing_bytes_for_hash(record.operation_hash),
            &record.signature,
        )
    }
}

/// Validates every per-node hash chain present in `operations`. Operations may
/// belong to multiple authoring nodes (a partition is written by many nodes);
/// each `actor_node_id` is validated independently as its own dense, monotonic
/// chain ordered by `node_seq`. The FIRST op seen for each node anchors that
/// node's chain — its `prev_node_hash` is not checked, because the predecessor
/// may live in a compacted snapshot prefix; only subsequent links within the
/// batch are verified. Equivocation — two distinct operations sharing
/// `(actor_node_id, node_seq)` — is a Byzantine fault and rejected outright.
pub fn validate_hash_chain(operations: &[SyncOperation]) -> LedgerResult<()> {
    validate_per_node_chain(operations, &BTreeMap::new())
}

/// As `validate_hash_chain`, but `expected_previous_hash` seeds the chain of the
/// SINGLE node whose operations follow on a snapshot/pull tail: the first op MUST
/// chain onto that seed. Mixing several nodes with a non-None seed is meaningless,
/// so a seed only applies when all operations share one `actor_node_id`.
pub fn validate_hash_chain_from(
    operations: &[SyncOperation],
    expected_previous_hash: Option<[u8; 32]>,
) -> LedgerResult<()> {
    let mut seeds: BTreeMap<String, [u8; 32]> = BTreeMap::new();
    if let (Some(seed), Some(first)) = (expected_previous_hash, operations.first()) {
        seeds.insert(first.body.actor_node_id.clone(), seed);
    }
    validate_per_node_chain(operations, &seeds)
}

/// Anchors a multi-writer tail onto an attested per-node frontier. For every
/// authoring node already present in `frontier`, that node's FIRST op in the tail
/// MUST chain onto `frontier[node].hash` (the attested chain tip) — this rejects a
/// relay that splices an authentically signed but stale/forked segment whose
/// `prev_node_hash` does not continue the donor's chain (CR-W1). A writer absent
/// from the frontier is new to the receiver: its first op anchors at genesis (the
/// predecessor may live in a compacted prefix), exactly as `validate_hash_chain`.
pub fn validate_hash_chain_anchored(
    operations: &[SyncOperation],
    frontier: &BTreeMap<String, (u64, [u8; 32])>,
) -> LedgerResult<()> {
    let seeds: BTreeMap<String, [u8; 32]> = frontier
        .iter()
        .map(|(node_id, (_, hash))| (node_id.clone(), *hash))
        .collect();
    validate_per_node_chain(operations, &seeds)
}

fn validate_per_node_chain(
    operations: &[SyncOperation],
    seeds: &BTreeMap<String, [u8; 32]>,
) -> LedgerResult<()> {
    // (actor_node_id, node_seq) -> op_hash, to catch equivocation across the
    // whole batch, and last accepted hash per node to link the chain.
    let mut seen: BTreeMap<(String, u64), [u8; 32]> = BTreeMap::new();
    let mut sorted: Vec<&SyncOperation> = operations.iter().collect();
    sorted.sort_by(|a, b| {
        a.body
            .actor_node_id
            .cmp(&b.body.actor_node_id)
            .then_with(|| a.body.node_seq.cmp(&b.body.node_seq))
    });

    let mut current_node: Option<&str> = None;
    let mut previous_hash: Option<[u8; 32]> = None;
    for operation in sorted {
        operation.validate_integrity()?;
        let node = operation.body.actor_node_id.as_str();
        if let Some(existing) = seen.insert(
            (node.to_string(), operation.body.node_seq),
            operation.operation_hash,
        ) {
            if existing != operation.operation_hash {
                return Err(SyncLedgerError::NodeEquivocation {
                    node: node.to_string(),
                    node_seq: operation.body.node_seq,
                    existing: OperationId::from_hash(existing),
                    incoming: operation.op_id,
                });
            }
        }
        let first_of_node = current_node != Some(node);
        if first_of_node {
            current_node = Some(node);
            // With a seed for this node the first op must chain onto it; without
            // one the first op anchors the chain (predecessor may be compacted
            // away). Seeds are keyed per authoring node so an interleaved
            // multi-writer tail anchors each writer onto its own attested tip.
            if let Some(seed_hash) = seeds.get(node) {
                if operation.body.prev_node_hash != Some(*seed_hash) {
                    return Err(SyncLedgerError::HashChainMismatch {
                        node: node.to_string(),
                        node_seq: operation.body.node_seq,
                    });
                }
            }
        } else if operation.body.prev_node_hash != previous_hash {
            return Err(SyncLedgerError::HashChainMismatch {
                node: node.to_string(),
                node_seq: operation.body.node_seq,
            });
        }
        previous_hash = Some(operation.operation_hash);
    }
    Ok(())
}

/// Builds a Merkle summary over one partition's operations. A partition is now
/// written by several nodes, so there is no global partition sequence: the
/// `from_sequence`/`to_sequence` fields are a 1-based count watermark over the
/// HLC-ordered operation set (1..=N). The per-node hash chains are validated by
/// the caller via `validate_hash_chain`; here the input must all share one
/// partition and the leaves are taken in HLC order so the root is deterministic.
pub fn build_merkle_summary(operations: &[SyncOperation]) -> LedgerResult<SyncMerkleSummary> {
    let first = operations
        .first()
        .ok_or(SyncLedgerError::EmptyMerkleSummary)?;
    let partition = first.body.partition_id.clone();

    let mut ordered: Vec<&SyncOperation> = operations.iter().collect();
    ordered.sort_by(|a, b| super::types::partition_materialization_order(a, b));

    let mut leaves = Vec::with_capacity(ordered.len());
    for operation in &ordered {
        operation.validate_integrity()?;
        if operation.body.partition_id != partition {
            return Err(SyncLedgerError::MerklePartitionMismatch {
                expected: partition.as_str().to_string(),
                actual: operation.body.partition_id.as_str().to_string(),
            });
        }
        leaves.push(hash_leaf(operation.operation_hash));
    }

    let operation_count = ordered.len() as u64;
    Ok(SyncMerkleSummary {
        partition_id: partition,
        from_sequence: 1,
        to_sequence: operation_count,
        operation_count,
        root_hash: merkle_root(leaves),
    })
}

/// Per-node coverage frontier over an operation set: for every authoring node
/// present, the highest `node_seq` and that operation's `operation_hash`. The
/// caller validates the per-node chains first (`validate_hash_chain`), so the
/// highest `node_seq` per node is the dense tip of a contiguous chain — exactly
/// the frontier a snapshot attests and a receiver advances to. Deterministic:
/// it depends only on the (node, seq) maxima, never on insertion order.
pub fn node_frontier_for_operations(
    operations: &[SyncOperation],
) -> BTreeMap<String, (u64, [u8; 32])> {
    let mut frontier: BTreeMap<String, (u64, [u8; 32])> = BTreeMap::new();
    for operation in operations {
        let entry = frontier
            .entry(operation.body.actor_node_id.clone())
            .or_insert((0, [0u8; 32]));
        if operation.body.node_seq >= entry.0 {
            *entry = (operation.body.node_seq, operation.operation_hash);
        }
    }
    frontier
}

fn parse_verifying_key(actor_node_id: &str, key_hex: &str) -> LedgerResult<VerifyingKey> {
    let key_bytes = hex::decode(key_hex).map_err(|_| SyncLedgerError::InvalidPublicKey {
        actor_node_id: actor_node_id.to_string(),
    })?;
    let key_array: [u8; 32] =
        key_bytes
            .try_into()
            .map_err(|_| SyncLedgerError::InvalidPublicKey {
                actor_node_id: actor_node_id.to_string(),
            })?;
    VerifyingKey::from_bytes(&key_array).map_err(|_| SyncLedgerError::InvalidPublicKey {
        actor_node_id: actor_node_id.to_string(),
    })
}

fn verify_signature(
    actor_node_id: &str,
    key: &VerifyingKey,
    message: &[u8],
    signature: &[u8],
) -> LedgerResult<()> {
    let signature_array: [u8; 64] =
        signature
            .try_into()
            .map_err(|_| SyncLedgerError::InvalidSignatureLength {
                len: signature.len(),
            })?;
    let signature = Signature::from_bytes(&signature_array);
    key.verify(message, &signature)
        .map_err(|_| SyncLedgerError::InvalidSignature {
            actor_node_id: actor_node_id.to_string(),
        })
}

fn hash_leaf(operation_hash: [u8; 32]) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(MERKLE_LEAF_DOMAIN.len() + operation_hash.len());
    bytes.extend_from_slice(MERKLE_LEAF_DOMAIN);
    bytes.extend_from_slice(&operation_hash);
    *blake3::hash(&bytes).as_bytes()
}

fn hash_node(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(MERKLE_NODE_DOMAIN.len() + left.len() + right.len());
    bytes.extend_from_slice(MERKLE_NODE_DOMAIN);
    bytes.extend_from_slice(&left);
    bytes.extend_from_slice(&right);
    *blake3::hash(&bytes).as_bytes()
}

fn merkle_root(mut level: Vec<[u8; 32]>) -> [u8; 32] {
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let left = pair[0];
            let right = pair.get(1).copied().unwrap_or(left);
            next.push(hash_node(left, right));
        }
        level = next;
    }
    level[0]
}

pub fn operation_body_hash(operation: &SyncOperation) -> LedgerResult<[u8; 32]> {
    hash_canonical(&operation.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::ledger::{
        ActionType, FieldValue, HybridLogicalTimestamp, NewSyncOperation, PartitionId,
        SyncOperation,
    };
    use rand_core_06::OsRng;

    fn signer() -> Ed25519OperationSigner {
        let signing_key = SigningKey::generate(&mut OsRng);
        let node_id = hex::encode(signing_key.verifying_key().to_bytes());
        Ed25519OperationSigner::new(node_id, signing_key).unwrap()
    }

    fn operation(
        signer: &Ed25519OperationSigner,
        sequence: u64,
        previous_hash: Option<[u8; 32]>,
    ) -> SyncOperation {
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert("name".to_string(), FieldValue::String("Jan".to_string()));
        SyncOperation::from_new(
            NewSyncOperation {
                org_id: "org_1".to_string(),
                partition_id: PartitionId::new("addon/contacts/persons").unwrap(),
                addon_id: "contacts".to_string(),
                resource_type: "person".to_string(),
                resource_id: format!("person_{sequence}"),
                table_name: "persons".to_string(),
                primary_key: format!("person_{sequence}"),
                action: ActionType::Insert,
                changed_fields,
                before_hash: None,
                after_hash: Some([7; 32]),
                actor_user_id: "user_1".to_string(),
                actor_device_id: "device_1".to_string(),
                actor_node_id: signer.node_id().to_string(),
                hlc_timestamp: HybridLogicalTimestamp {
                    wall_time_ms: 1_765_000_000_000,
                    logical: sequence as u32,
                    node_id: signer.node_id().to_string(),
                },
                epoch: crate::sync::ledger::BaselineEpoch {
                    counter: 0,
                    origin_node: String::new(),
                },
                environment: crate::sync::ledger::NodeEnvironment::default(),
                payload_hash: [1; 32],
                acl_snapshot_hash: [2; 32],
                policy_epoch: 1,
                encryption_info: None,
            },
            sequence,
            previous_hash,
            signer,
        )
        .unwrap()
    }

    #[test]
    fn verifier_accepts_valid_operation_signature() {
        let signer = signer();
        let verifier = HexNodeIdOperationVerifier;
        let operation = operation(&signer, 1, None);

        verifier.verify_operation_signature(&operation).unwrap();
    }

    #[test]
    fn verifier_rejects_tampered_operation_hash() {
        let signer = signer();
        let verifier = HexNodeIdOperationVerifier;
        let mut operation = operation(&signer, 1, None);
        operation.body.resource_id = "tampered".to_string();

        assert!(matches!(
            verifier.verify_operation_signature(&operation),
            Err(SyncLedgerError::InvalidOperationHash { .. })
        ));
    }

    #[test]
    fn hash_chain_rejects_wrong_previous_hash() {
        let signer = signer();
        let first = operation(&signer, 1, None);
        let second = operation(&signer, 2, Some([9; 32]));

        assert!(matches!(
            validate_hash_chain(&[first, second]),
            Err(SyncLedgerError::HashChainMismatch { .. })
        ));
    }

    #[test]
    fn hash_chain_rejects_node_equivocation() {
        // Two distinct operations the same node minted at the same node_seq: a
        // single-writer node can only produce this by signing two histories.
        let signer = signer();
        let first = operation(&signer, 1, None);
        let forked = operation(&signer, 1, Some([4; 32]));
        assert_ne!(first.operation_hash, forked.operation_hash);

        assert!(matches!(
            validate_hash_chain(&[first, forked]),
            Err(SyncLedgerError::NodeEquivocation { node_seq: 1, .. })
        ));
    }

    #[test]
    fn hash_chain_anchors_first_op_without_seed() {
        // The first op of a node anchors its chain — its prev_node_hash is not
        // checked, because the predecessor may live in a compacted prefix.
        let signer = signer();
        let tail = operation(&signer, 5, Some([7; 32]));
        validate_hash_chain(&[tail]).unwrap();
    }

    #[test]
    fn hash_chain_accepts_range_with_known_previous_hash() {
        let signer = signer();
        let first = operation(&signer, 1, None);
        let second = operation(&signer, 2, Some(first.operation_hash));

        validate_hash_chain_from(&[second], Some(first.operation_hash)).unwrap();
    }

    fn redacted_from(operation: &SyncOperation) -> RedactedRecord {
        RedactedRecord {
            op_id: operation.op_id,
            operation_hash: operation.operation_hash,
            actor_node_id: operation.body.actor_node_id.clone(),
            node_seq: operation.body.node_seq,
            prev_node_hash: operation.body.prev_node_hash,
            signature: operation.signature.clone(),
        }
    }

    #[test]
    fn verifier_accepts_redacted_signature_over_op_id() {
        // The body is gone, but the signature is over operation_hash (== op_id), so
        // a redacted placeholder is still fully signature-verifiable.
        let signer = signer();
        let verifier = HexNodeIdOperationVerifier;
        let operation = operation(&signer, 1, None);
        let record = redacted_from(&operation);

        verifier.verify_redacted_signature(&record).unwrap();
    }

    #[test]
    fn verifier_rejects_redacted_with_mismatched_op_id() {
        // A redacted record whose op_id does not equal its carried operation_hash
        // is rejected before any signature check — the receiver keys its chain by
        // op_id, so this binding must hold.
        let signer = signer();
        let verifier = HexNodeIdOperationVerifier;
        let operation = operation(&signer, 1, None);
        let mut record = redacted_from(&operation);
        record.op_id = OperationId::from_hash([0xAB; 32]);

        assert!(matches!(
            verifier.verify_redacted_signature(&record),
            Err(SyncLedgerError::InvalidOperationId { .. })
        ));
    }

    #[test]
    fn verifier_rejects_redacted_with_forged_signature() {
        let signer = signer();
        let verifier = HexNodeIdOperationVerifier;
        let operation = operation(&signer, 1, None);
        let mut record = redacted_from(&operation);
        record.signature = vec![0u8; 64];

        assert!(matches!(
            verifier.verify_redacted_signature(&record),
            Err(SyncLedgerError::InvalidSignature { .. })
        ));
    }

    #[test]
    fn merkle_summary_covers_contiguous_operations() {
        let signer = signer();
        let first = operation(&signer, 1, None);
        let second = operation(&signer, 2, Some(first.operation_hash));

        let summary = build_merkle_summary(&[first.clone(), second.clone()]).unwrap();
        let repeated = build_merkle_summary(&[first, second]).unwrap();

        assert_eq!(summary.operation_count, 2);
        assert_eq!(summary.from_sequence, 1);
        assert_eq!(summary.to_sequence, 2);
        assert_eq!(summary.root_hash, repeated.root_hash);
    }
}
