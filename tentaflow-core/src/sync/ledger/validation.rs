// =============================================================================
// Plik: sync/ledger/validation.rs
// Opis: Walidacja podpisow, integralnosci hash-chain i Merkle summary dla Sync Ledger.
// =============================================================================

use super::types::{
    hash_canonical, LedgerResult, SyncLedgerError, SyncMerkleSummary, SyncOperation,
    SyncOperationSigner, SyncOperationVerifier,
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
}

pub fn validate_hash_chain(operations: &[SyncOperation]) -> LedgerResult<()> {
    validate_hash_chain_from(operations, None)
}

pub fn validate_hash_chain_from(
    operations: &[SyncOperation],
    expected_previous_hash: Option<[u8; 32]>,
) -> LedgerResult<()> {
    let mut previous_hash = expected_previous_hash;
    for operation in operations {
        operation.validate_integrity()?;
        if operation.body.prev_partition_hash != previous_hash {
            return Err(SyncLedgerError::HashChainMismatch {
                partition: operation.body.partition_id.as_str().to_string(),
                sequence: operation.body.partition_sequence,
            });
        }
        previous_hash = Some(operation.operation_hash);
    }
    Ok(())
}

pub fn build_merkle_summary(operations: &[SyncOperation]) -> LedgerResult<SyncMerkleSummary> {
    let first = operations
        .first()
        .ok_or(SyncLedgerError::EmptyMerkleSummary)?;
    let partition = first.body.partition_id.clone();
    let mut expected_sequence = first.body.partition_sequence;
    let mut leaves = Vec::with_capacity(operations.len());

    for operation in operations {
        operation.validate_integrity()?;
        if operation.body.partition_id != partition {
            return Err(SyncLedgerError::MerklePartitionMismatch {
                expected: partition.as_str().to_string(),
                actual: operation.body.partition_id.as_str().to_string(),
            });
        }
        if operation.body.partition_sequence != expected_sequence {
            return Err(SyncLedgerError::MerkleSequenceGap {
                expected: expected_sequence,
                actual: operation.body.partition_sequence,
            });
        }
        leaves.push(hash_leaf(operation.operation_hash));
        expected_sequence = expected_sequence.saturating_add(1);
    }

    let to_sequence = operations
        .last()
        .map(|operation| operation.body.partition_sequence)
        .ok_or(SyncLedgerError::EmptyMerkleSummary)?;
    let operation_count = operations.len() as u64;
    Ok(SyncMerkleSummary {
        partition_id: partition,
        from_sequence: first.body.partition_sequence,
        to_sequence,
        operation_count,
        root_hash: merkle_root(leaves),
    })
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
    fn hash_chain_accepts_range_with_known_previous_hash() {
        let signer = signer();
        let first = operation(&signer, 1, None);
        let second = operation(&signer, 2, Some(first.operation_hash));

        validate_hash_chain_from(&[second], Some(first.operation_hash)).unwrap();
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
