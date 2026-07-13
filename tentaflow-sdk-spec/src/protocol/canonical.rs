// =============================================================================
// File: protocol/canonical.rs — strict canonical CBOR wire validator
//
// Implements the **Faza 6 deterministic profile**, a stricter superset of
// RFC 8949 §4.2.1 Core Deterministic Encoding:
//
//   *Standard (RFC §4.2.1 base)*
//     - Definite-length items only (no 0x5F/7F/9F/BF/FF anywhere).
//     - Minimum-width argument encoding for all integer head arguments,
//       including unsigned/negative ints, bstr/tstr lengths, array/map
//       element counts, CBOR tag numbers.
//     - Map keys in **strictly increasing bytewise order over the
//       encoded key form** (RFC 8949 §4.2.1, NOT the §4.2.3 length-first
//       variant). For our protocol — which only uses pure-u8 integer-key
//       maps and pure-tstr-key maps — both orderings agree byte-for-byte,
//       but we implement true bytewise for forwards compatibility.
//     - No duplicate map keys.
//
//   *Faza 6 additions on top of RFC §4.2.1*
//     - Floats MUST be encoded as f64 (head 0xFB). f16 / f32 are rejected
//       outright because the catalog declares every numeric payload as
//       f64. (RFC core would also accept the shortest exact float; we
//       narrow that to f64-only.)
//     - Simple value `undefined` (0xF7), 1-byte simple values (head 0xF8)
//       and reserved simple values 28..=30 are rejected — the catalog has
//       no use for them.
//
//   *Operational hardening*
//     - Recursion is bounded by `MAX_NESTING_DEPTH = 64`. Untrusted
//       payloads cannot stack-overflow the process via deeply nested
//       arrays/maps/tags.
//     - All length conversions use `usize::try_from`, which on 32-bit
//       targets (`wasm32`) rejects items whose encoded length exceeds
//       `usize::MAX` instead of silently truncating.
//
// The validator is stateless: it walks the byte stream once. Higher-level
// schema validation (Krok 4b) builds on this — once we know the byte
// stream is canonical, we can safely decode and check field semantics.
//
// API:
//   validate_canonical(bytes) -> Result<(), CanonicalError>
// =============================================================================

use core::fmt;

/// Kind of canonical-encoding violation. See module docs for semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalErrorKind {
    IndefiniteLength,
    NonCanonicalIntegerWidth,
    NonCanonicalFloatWidth,
    NonCanonicalKeyOrder,
    DuplicateMapKey,
    TruncatedInput,
    InvalidMajorType,
    TrailingBytes,
    /// Item length cannot be addressed on the host pointer width
    /// (relevant on 32-bit / wasm32 targets).
    LengthOverflow,
    /// Nesting exceeded `MAX_NESTING_DEPTH`. Guards against stack DoS.
    NestingTooDeep,
}

/// Maximum allowed nesting depth for definite-length arrays / maps / tag
/// wrappers. Conservative: 64 levels covers all catalog payloads (typical
/// max is ~6, deepest reasonable case under recursive Handler trees is
/// well below 32).
pub const MAX_NESTING_DEPTH: u32 = 64;

/// Canonical-encoding rejection. `byte_offset` points at the start of the
/// offending CBOR head byte (or at the truncation point for
/// `TruncatedInput`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalError {
    pub kind: CanonicalErrorKind,
    pub byte_offset: usize,
    pub message: &'static str,
}

impl fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "canonical CBOR violation at byte {}: {:?} — {}",
            self.byte_offset, self.kind, self.message,
        )
    }
}

impl std::error::Error for CanonicalError {}

/// Validate the byte sequence against RFC 8949 §4.2 Core Deterministic
/// Encoding plus the Faza 6 catalog conventions described in the module
/// docs. `Ok(())` means the payload is canonical; any defect yields
/// `Err(CanonicalError)`.
pub fn validate_canonical(bytes: &[u8]) -> Result<(), CanonicalError> {
    let mut cursor = Cursor {
        bytes,
        pos: 0,
        depth: 0,
    };
    cursor.read_item()?;
    if cursor.pos != bytes.len() {
        return Err(CanonicalError {
            kind: CanonicalErrorKind::TrailingBytes,
            byte_offset: cursor.pos,
            message: "extra bytes after top-level CBOR item",
        });
    }
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Current nesting level — incremented before recursing into an array,
    /// map or tag, decremented on the way out. Compared to
    /// `MAX_NESTING_DEPTH` before each recursion.
    depth: u32,
}

impl<'a> Cursor<'a> {
    fn peek_at(&self, offset: usize) -> Result<u8, CanonicalError> {
        self.bytes.get(offset).copied().ok_or(CanonicalError {
            kind: CanonicalErrorKind::TruncatedInput,
            byte_offset: offset,
            message: "input ended unexpectedly",
        })
    }

    fn read_u8(&mut self) -> Result<u8, CanonicalError> {
        let b = self.peek_at(self.pos)?;
        self.pos += 1;
        Ok(b)
    }

    fn enter(&mut self, head_offset: usize) -> Result<(), CanonicalError> {
        if self.depth >= MAX_NESTING_DEPTH {
            return Err(CanonicalError {
                kind: CanonicalErrorKind::NestingTooDeep,
                byte_offset: head_offset,
                message: "CBOR nesting exceeds MAX_NESTING_DEPTH",
            });
        }
        self.depth += 1;
        Ok(())
    }

    fn exit(&mut self) {
        debug_assert!(self.depth > 0);
        self.depth -= 1;
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], CanonicalError> {
        let end = self.pos.checked_add(n).ok_or(CanonicalError {
            kind: CanonicalErrorKind::TruncatedInput,
            byte_offset: self.pos,
            message: "length overflow",
        })?;
        if end > self.bytes.len() {
            return Err(CanonicalError {
                kind: CanonicalErrorKind::TruncatedInput,
                byte_offset: self.pos,
                message: "tried to consume past end of input",
            });
        }
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    /// Walk one full CBOR data item starting at `self.pos`, advancing
    /// the cursor past it. Returns the slice that was consumed (used by
    /// the map-key ordering check).
    fn read_item(&mut self) -> Result<&'a [u8], CanonicalError> {
        let start = self.pos;
        let head = self.read_u8()?;
        let major = head >> 5;
        let info = head & 0b0001_1111;

        match major {
            0 | 1 => {
                // unsigned/negative int — value carried in `info` or
                // following bytes; enforce minimum-width.
                self.consume_canonical_argument(start, info)?;
            }
            2 | 3 => {
                // bstr / tstr — `info` encodes byte length.
                let len = self.consume_canonical_argument(start, info)?;
                let len_us = usize::try_from(len).map_err(|_| CanonicalError {
                    kind: CanonicalErrorKind::LengthOverflow,
                    byte_offset: start,
                    message: "string length exceeds usize on this target",
                })?;
                self.read_bytes(len_us)?;
            }
            4 => {
                // array — `info` encodes element count.
                let n = self.consume_canonical_argument(start, info)?;
                self.enter(start)?;
                for _ in 0..n {
                    self.read_item()?;
                }
                self.exit();
            }
            5 => {
                // map — `info` encodes pair count.
                let n = self.consume_canonical_argument(start, info)?;
                self.enter(start)?;
                let mut prev_key: Option<&[u8]> = None;
                for _ in 0..n {
                    let key_start = self.pos;
                    let key = self.read_item()?;
                    if let Some(prev) = prev_key {
                        match canonical_key_cmp(prev, key) {
                            core::cmp::Ordering::Less => { /* OK */ }
                            core::cmp::Ordering::Equal => {
                                return Err(CanonicalError {
                                    kind: CanonicalErrorKind::DuplicateMapKey,
                                    byte_offset: key_start,
                                    message: "same map key appears twice",
                                })
                            }
                            core::cmp::Ordering::Greater => {
                                return Err(CanonicalError {
                                    kind: CanonicalErrorKind::NonCanonicalKeyOrder,
                                    byte_offset: key_start,
                                    message:
                                        "map keys must be in strictly increasing bytewise order",
                                })
                            }
                        }
                    }
                    prev_key = Some(key);
                    // Value.
                    self.read_item()?;
                }
                self.exit();
            }
            6 => {
                // tag — `info` is the tag number; nested item must be canonical.
                self.consume_canonical_argument(start, info)?;
                self.enter(start)?;
                self.read_item()?;
                self.exit();
            }
            7 => {
                // simple/float — only specific encodings are canonical here.
                match info {
                    20..=23 => {
                        // simple values: false / true / null / undefined.
                        // Allow false (20), true (21), null (22). `undefined` (23)
                        // is rejected — Faza 6 catalog disallows it.
                        if info == 23 {
                            return Err(CanonicalError {
                                kind: CanonicalErrorKind::InvalidMajorType,
                                byte_offset: start,
                                message: "CBOR `undefined` (0xF7) is not allowed",
                            });
                        }
                    }
                    24 => {
                        // 1-byte simple value: only 32..=255 ranges are
                        // allowed by RFC, but the catalog has no use case,
                        // so we reject.
                        return Err(CanonicalError {
                            kind: CanonicalErrorKind::InvalidMajorType,
                            byte_offset: start,
                            message: "1-byte simple value not allowed (catalog uses only false/true/null + f64)",
                        });
                    }
                    25 | 26 => {
                        // f16 (0xF9) / f32 (0xFA) — Faza 6 numeric floats
                        // are encoded as f64. Reject narrower widths.
                        return Err(CanonicalError {
                            kind: CanonicalErrorKind::NonCanonicalFloatWidth,
                            byte_offset: start,
                            message: "float must be encoded as f64 (head 0xFB)",
                        });
                    }
                    27 => {
                        // f64. Consume the 8 payload bytes.
                        self.read_bytes(8)?;
                    }
                    28..=30 => {
                        return Err(CanonicalError {
                            kind: CanonicalErrorKind::InvalidMajorType,
                            byte_offset: start,
                            message: "reserved/illegal simple-value head",
                        });
                    }
                    31 => {
                        // `break` (0xFF) outside an indefinite-length item.
                        return Err(CanonicalError {
                            kind: CanonicalErrorKind::IndefiniteLength,
                            byte_offset: start,
                            message: "stray indefinite-length break byte",
                        });
                    }
                    _ => {
                        // info 0..=19 — direct simple values; catalog
                        // doesn't use them.
                        return Err(CanonicalError {
                            kind: CanonicalErrorKind::InvalidMajorType,
                            byte_offset: start,
                            message:
                                "unsupported simple value (catalog uses only false/true/null + f64)",
                        });
                    }
                }
            }
            _ => unreachable!("major type is 3 bits"),
        }
        Ok(&self.bytes[start..self.pos])
    }

    /// Consume an integer argument with canonical-width enforcement.
    /// Returns the decoded u64 value. `info` is the 5-bit head argument.
    fn consume_canonical_argument(
        &mut self,
        head_offset: usize,
        info: u8,
    ) -> Result<u64, CanonicalError> {
        match info {
            0..=23 => Ok(u64::from(info)),
            24 => {
                let v = self.read_u8()?;
                if v < 24 {
                    return Err(CanonicalError {
                        kind: CanonicalErrorKind::NonCanonicalIntegerWidth,
                        byte_offset: head_offset,
                        message: "1-byte argument fits in 5-bit head; minimum-width required",
                    });
                }
                Ok(u64::from(v))
            }
            25 => {
                let v = u16::from_be_bytes(self.read_bytes(2)?.try_into().unwrap());
                if v <= u16::from(u8::MAX) {
                    return Err(CanonicalError {
                        kind: CanonicalErrorKind::NonCanonicalIntegerWidth,
                        byte_offset: head_offset,
                        message: "2-byte argument fits in 1-byte form; minimum-width required",
                    });
                }
                Ok(u64::from(v))
            }
            26 => {
                let v = u32::from_be_bytes(self.read_bytes(4)?.try_into().unwrap());
                if v <= u32::from(u16::MAX) {
                    return Err(CanonicalError {
                        kind: CanonicalErrorKind::NonCanonicalIntegerWidth,
                        byte_offset: head_offset,
                        message: "4-byte argument fits in 2-byte form; minimum-width required",
                    });
                }
                Ok(u64::from(v))
            }
            27 => {
                let v = u64::from_be_bytes(self.read_bytes(8)?.try_into().unwrap());
                if v <= u64::from(u32::MAX) {
                    return Err(CanonicalError {
                        kind: CanonicalErrorKind::NonCanonicalIntegerWidth,
                        byte_offset: head_offset,
                        message: "8-byte argument fits in 4-byte form; minimum-width required",
                    });
                }
                Ok(v)
            }
            28..=30 => Err(CanonicalError {
                kind: CanonicalErrorKind::InvalidMajorType,
                byte_offset: head_offset,
                message: "reserved argument width",
            }),
            31 => Err(CanonicalError {
                kind: CanonicalErrorKind::IndefiniteLength,
                byte_offset: head_offset,
                message: "indefinite-length item forbidden by catalog",
            }),
            _ => unreachable!("info is 5 bits"),
        }
    }
}

/// RFC 8949 §4.2.1 Core Deterministic Encoding map-key ordering:
/// strictly increasing bytewise lexicographic comparison over the encoded
/// key form. This is **not** the §4.2.3 length-first variant — length
/// only matters because the first byte of every encoded item embeds the
/// major type and (for short forms) the length argument, so for our
/// protocol — which only uses pure-u8-keyed or pure-tstr-keyed maps —
/// both orderings produce the same sequence. We implement true bytewise
/// for forward compatibility with future mixed-type-key maps.
fn canonical_key_cmp(a: &[u8], b: &[u8]) -> core::cmp::Ordering {
    a.cmp(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: encode a single u8 head byte + no payload.
    fn head_only(major: u8, info: u8) -> Vec<u8> {
        vec![(major << 5) | (info & 0b0001_1111)]
    }

    #[test]
    fn accepts_small_unsigned_int() {
        // 0x00 .. 0x17 are direct small unsigned ints.
        for v in 0u8..=23 {
            assert!(validate_canonical(&[v]).is_ok(), "u={v}");
        }
    }

    #[test]
    fn accepts_canonical_1byte_unsigned() {
        // 0x18 0x18 = encoded form of u8 24 (smallest needing 1-byte arg).
        assert!(validate_canonical(&[0x18, 0x18]).is_ok());
    }

    #[test]
    fn rejects_noncanonical_1byte_unsigned() {
        // 0x18 0x05 — value 5 should be a direct 5-bit head (0x05).
        let err = validate_canonical(&[0x18, 0x05]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalIntegerWidth);
        assert_eq!(err.byte_offset, 0);
    }

    #[test]
    fn rejects_noncanonical_2byte_unsigned() {
        // 0x19 0x00 0x18 — value 24 should fit in 1-byte form (0x18 0x18).
        let err = validate_canonical(&[0x19, 0x00, 0x18]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalIntegerWidth);
    }

    #[test]
    fn rejects_noncanonical_4byte_unsigned() {
        // 0x1A 0x00 0x00 0x01 0x00 — value 256 should fit in 2-byte form.
        let err = validate_canonical(&[0x1A, 0x00, 0x00, 0x01, 0x00]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalIntegerWidth);
    }

    #[test]
    fn accepts_canonical_4byte_unsigned() {
        // 0x1A 0x00 0x01 0x00 0x00 — value 65536 = 0x10000, smallest 4-byte.
        assert!(validate_canonical(&[0x1A, 0x00, 0x01, 0x00, 0x00]).is_ok());
    }

    #[test]
    fn rejects_indefinite_length_array() {
        // 0x9F = indefinite array.
        let err = validate_canonical(&[0x9F, 0x01, 0xFF]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::IndefiniteLength);
    }

    #[test]
    fn rejects_indefinite_length_map() {
        // 0xBF = indefinite map.
        let err = validate_canonical(&[0xBF, 0x01, 0x02, 0xFF]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::IndefiniteLength);
    }

    #[test]
    fn rejects_indefinite_length_tstr() {
        // 0x7F = indefinite-length text string.
        let err = validate_canonical(&[0x7F, 0x60, 0xFF]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::IndefiniteLength);
    }

    #[test]
    fn rejects_indefinite_length_bstr() {
        let err = validate_canonical(&[0x5F, 0x40, 0xFF]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::IndefiniteLength);
    }

    #[test]
    fn rejects_f16() {
        // 0xF9 = half float.
        let err = validate_canonical(&[0xF9, 0x00, 0x00]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalFloatWidth);
    }

    #[test]
    fn rejects_f32() {
        // 0xFA = single float.
        let err = validate_canonical(&[0xFA, 0x00, 0x00, 0x00, 0x00]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalFloatWidth);
    }

    #[test]
    fn accepts_f64() {
        // 0xFB followed by 8 zero bytes = 0.0 f64.
        assert!(validate_canonical(&[0xFB, 0, 0, 0, 0, 0, 0, 0, 0]).is_ok());
    }

    #[test]
    fn accepts_simple_false_true_null() {
        assert!(validate_canonical(&[0xF4]).is_ok()); // false
        assert!(validate_canonical(&[0xF5]).is_ok()); // true
        assert!(validate_canonical(&[0xF6]).is_ok()); // null
    }

    #[test]
    fn rejects_simple_undefined() {
        let err = validate_canonical(&[0xF7]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::InvalidMajorType);
    }

    #[test]
    fn accepts_canonical_map_with_two_keys() {
        // {0:1, 1:2}
        let bytes = vec![
            0xA2, // map(2)
            0x00, 0x01, // 0 → 1
            0x01, 0x02, // 1 → 2
        ];
        assert!(validate_canonical(&bytes).is_ok());
    }

    #[test]
    fn rejects_non_canonical_map_key_order() {
        // {1:1, 0:2} — keys in wrong order.
        let bytes = vec![0xA2, 0x01, 0x01, 0x00, 0x02];
        let err = validate_canonical(&bytes).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalKeyOrder);
    }

    #[test]
    fn rejects_duplicate_map_key() {
        // {0:1, 0:2}
        let bytes = vec![0xA2, 0x00, 0x01, 0x00, 0x02];
        let err = validate_canonical(&bytes).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::DuplicateMapKey);
    }

    #[test]
    fn canonical_key_cmp_shorter_first() {
        // "a" (0x61 0x61) sorts before "ab" (0x62 0x61 0x62) — shorter form.
        assert_eq!(
            canonical_key_cmp(&[0x61, 0x61], &[0x62, 0x61, 0x62]),
            core::cmp::Ordering::Less,
        );
    }

    #[test]
    fn accepts_canonical_map_with_tstr_keys() {
        // {"a": 1, "b": 2}
        let bytes = vec![0xA2, 0x61, b'a', 0x01, 0x61, b'b', 0x02];
        assert!(validate_canonical(&bytes).is_ok());
    }

    #[test]
    fn rejects_non_canonical_tstr_key_order() {
        // {"b": 1, "a": 2} — descending order.
        let bytes = vec![0xA2, 0x61, b'b', 0x01, 0x61, b'a', 0x02];
        let err = validate_canonical(&bytes).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalKeyOrder);
    }

    #[test]
    fn rejects_trailing_bytes() {
        // Two top-level items.
        let bytes = vec![0x01, 0x02];
        let err = validate_canonical(&bytes).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::TrailingBytes);
    }

    #[test]
    fn rejects_truncated_input() {
        // 0x19 promises 2 bytes of length but only 1 follows.
        let err = validate_canonical(&[0x19, 0x01]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::TruncatedInput);
    }

    #[test]
    fn rejects_empty_input() {
        let err = validate_canonical(&[]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::TruncatedInput);
    }

    #[test]
    fn rejects_noncanonical_array_length() {
        // 0x98 0x05 = array of length 5 with 1-byte form, but length 5 fits
        // in 5-bit head (0x85). Non-canonical.
        let err = validate_canonical(&[0x98, 0x05]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalIntegerWidth);
    }

    #[test]
    fn rejects_noncanonical_tstr_length() {
        // 0x78 0x05 + 5 bytes = tstr of length 5 with non-canonical width.
        let err = validate_canonical(&[0x78, 0x05, b'h', b'e', b'l', b'l', b'o']).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalIntegerWidth);
    }

    #[test]
    fn accepts_negative_int_small_form() {
        // 0x29 = -10 (info 9 → value -1-9 = -10).
        assert!(validate_canonical(&[0x29]).is_ok());
    }

    #[test]
    fn rejects_noncanonical_negative_int() {
        // 0x38 0x05 = -(5+1) = -6, but should be direct head 0x25.
        let err = validate_canonical(&[0x38, 0x05]).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NonCanonicalIntegerWidth);
    }

    #[test]
    fn accepts_nested_canonical_structure() {
        // {"k": [1, 2, "v"]}
        let bytes = vec![
            0xA1, // map(1)
            0x61, b'k', // "k"
            0x83, // array(3)
            0x01, 0x02, 0x61, b'v',
        ];
        assert!(validate_canonical(&bytes).is_ok());
    }

    #[test]
    fn rejects_excessive_nesting() {
        // Build (MAX+1) levels of nested 1-element arrays: 0x81 0x81 ... 0x00.
        let mut bytes: Vec<u8> = Vec::new();
        for _ in 0..=MAX_NESTING_DEPTH as usize {
            bytes.push(0x81); // array(1)
        }
        bytes.push(0x00); // final value (small uint 0)
        let err = validate_canonical(&bytes).unwrap_err();
        assert_eq!(err.kind, CanonicalErrorKind::NestingTooDeep);
    }

    #[test]
    fn accepts_nesting_at_limit() {
        // Exactly MAX_NESTING_DEPTH levels of array(1) wrapping is allowed.
        let mut bytes: Vec<u8> = Vec::new();
        for _ in 0..MAX_NESTING_DEPTH as usize {
            bytes.push(0x81);
        }
        bytes.push(0x00);
        assert!(validate_canonical(&bytes).is_ok());
    }

    #[test]
    fn canonical_key_cmp_is_pure_bytewise() {
        // 0x18 0x64 (=100) vs 0x20 (=-1): bytewise says 100 < -1
        // (0x18 < 0x20). Length-first would say -1 < 100 (shorter first).
        // We use bytewise.
        let key_100: &[u8] = &[0x18, 0x64];
        let key_neg1: &[u8] = &[0x20];
        assert_eq!(
            canonical_key_cmp(key_100, key_neg1),
            core::cmp::Ordering::Less
        );
    }

    #[test]
    fn head_only_helper_smoke() {
        // major=0, info=5 → 0x05
        assert_eq!(head_only(0, 5), vec![0x05]);
        // major=4, info=2 → 0x82 (array of 2 — but truncated, won't validate).
        assert_eq!(head_only(4, 2), vec![0x82]);
    }
}
