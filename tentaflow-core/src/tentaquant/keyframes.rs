// ===== File: tentaquant/keyframes.rs — the recorded evolution of a run =====
//
// A T1 run may record one [`StateKeyframe`] per gate (plan §13.6, "record
// evolution"): Bloch vectors, purity, the reduced density matrices of the
// gate's qubits, the heaviest amplitudes with the partners the gate mixed them
// with, and the heaviest bitstring probabilities. The browser interpolates
// between two keyframes exactly, which is why a frame carries the gate matrix
// and the partner amplitudes and not just a picture of the state.
//
// Two consumers, one producer:
//   * live — each frame goes out on the run stream as it is computed, so the
//     run view animates while the run is still executing;
//   * afterwards — the whole series is stored as ONE CBOR artifact in the
//     lab's content store and `runs.keyframes_sha256` points at it, so a
//     reader who never held the stream replays the same evolution.
//
// The conversion below is the ONLY place the crate's `Keyframe` becomes a wire
// `StateKeyframe`. Complex numbers travel as `[re, im]` rather than as the
// crate's `{re, im}` struct: it halves the CBOR of a 4×4 density matrix and it
// is what the browser's typed-array readers already expect.

use anyhow::{anyhow, Result};
use num_complex::Complex64;
use tentaflow_protocol::tentaquant::{
    KeyframeAmplitude, KeyframeGate, KeyframePair, KeyframePartner, KeyframeProbability,
    StateKeyframe,
};
use tentaflow_quantum::sim::statevector::{Keyframe, KeyframeOptions, PairSelection};

fn complex(value: Complex64) -> [f64; 2] {
    [value.re, value.im]
}

fn complexes(values: &[Complex64]) -> Vec<[f64; 2]> {
    values.iter().copied().map(complex).collect()
}

/// Wire form of one keyframe the simulator produced.
pub fn to_wire(frame: &Keyframe) -> StateKeyframe {
    StateKeyframe {
        step: frame.step as u32,
        gate: frame.gate.as_ref().map(|gate| KeyframeGate {
            name: gate.name.clone(),
            qubits: gate.qubits.iter().map(|q| *q as u32).collect(),
            matrix: complexes(&gate.matrix),
        }),
        bloch: frame.bloch.clone(),
        purity: frame.purity.clone(),
        pairs: frame
            .pairs
            .iter()
            .map(|pair| KeyframePair {
                qubits: [pair.qubits.0 as u32, pair.qubits.1 as u32],
                rho: complexes(&pair.rho),
                mutual_information: pair.mutual_information,
                concurrence: pair.concurrence,
            })
            .collect(),
        top: frame
            .top
            .iter()
            .map(|group| KeyframeAmplitude {
                index: group.index as u64,
                amplitude: complex(group.amplitude),
                partners: group
                    .partners
                    .iter()
                    .map(|(index, amplitude)| KeyframePartner {
                        index: *index as u64,
                        amplitude: complex(*amplitude),
                    })
                    .collect(),
            })
            .collect(),
        probs_top: frame
            .probs_top
            .iter()
            .map(|(bitstring, probability)| KeyframeProbability {
                bitstring: bitstring.clone(),
                probability: *probability,
            })
            .collect(),
    }
}

/// Keyframe budget of one run, from the options the caller sent. Unknown pair
/// selections fall back to the gate's own qubits — the default of plan §13.6,
/// and the only selection whose cost does not grow with the register.
pub fn options(top_k: u32, probs_top: u32, pairs: &str) -> KeyframeOptions {
    KeyframeOptions {
        pairs: match pairs {
            "none" => PairSelection::None,
            "all" => PairSelection::All,
            _ => PairSelection::GateQubits,
        },
        top_k: top_k as usize,
        probs_top: probs_top as usize,
    }
}

/// The whole series as ONE CBOR document, the artifact stored in the content
/// store. Same codec as the protocol frames, so a keyframe read back from the
/// store decodes into exactly the value that was streamed.
pub fn encode_bundle(frames: &[StateKeyframe]) -> Result<Vec<u8>> {
    tentaflow_protocol::cbor::encode(&frames).map_err(|e| anyhow!("keyframe bundle encode: {e}"))
}

/// Reads a stored series back.
pub fn decode_bundle(bytes: &[u8]) -> Result<Vec<StateKeyframe>> {
    tentaflow_protocol::cbor::decode::<Vec<StateKeyframe>>(bytes)
        .map_err(|e| anyhow!("keyframe bundle decode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_quantum::parse::{parse_qasm3, InputValues};
    use tentaflow_quantum::sim::statevector::{SimOptions, Simulator};
    use tentaflow_quantum::sim::Device;

    const BELL: &str = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nbit[2] c;\n\
                        h q[0];\ncx q[0], q[1];\nc = measure q;\n";

    /// The stored artifact and the live frame are the same value: the CBOR
    /// round trip is lossless, and the wire frame carries exactly what the
    /// crate computed for that step.
    #[test]
    fn a_bundle_round_trips_and_matches_the_simulator() {
        let circuit = parse_qasm3(BELL, &InputValues::new()).expect("parses");
        let mut simulator = Simulator::with_device(&circuit, &SimOptions::default(), Device::Cpu)
            .expect("simulator starts");
        let budget = options(256, 16, "gate");

        let mut wire = Vec::new();
        let mut native = Vec::new();
        while simulator.step() {
            let frame = simulator.keyframe(&budget).expect("keyframe");
            wire.push(to_wire(&frame));
            native.push(frame);
        }
        assert_eq!(wire.len(), circuit.ops().len());

        let bytes = encode_bundle(&wire).expect("encode");
        let decoded = decode_bundle(&bytes).expect("decode");
        assert_eq!(decoded, wire);

        // The Hadamard leaves qubit 0 on the equator; the frame the browser
        // gets says so with the same numbers the simulator holds.
        let after_h = &decoded[0];
        assert_eq!(after_h.step, 1);
        assert_eq!(
            after_h.gate.as_ref().map(|g| g.name.as_str()),
            Some(native[0].gate.as_ref().expect("gate").name.as_str())
        );
        assert!((after_h.bloch[0][0] - 1.0).abs() < 1e-12);
        assert!(after_h.bloch[0][2].abs() < 1e-12);

        // After the entangler both qubits are maximally mixed and the pair
        // carries the entanglement numbers the map draws.
        let after_cx = &decoded[1];
        assert_eq!(after_cx.pairs.len(), 1);
        assert!((after_cx.pairs[0].concurrence - 1.0).abs() < 1e-9);
        assert!(after_cx.purity[0] < 0.51);
    }

    #[test]
    fn pair_selection_follows_the_requested_budget() {
        assert!(matches!(options(8, 4, "none").pairs, PairSelection::None));
        assert!(matches!(options(8, 4, "all").pairs, PairSelection::All));
        assert!(matches!(
            options(8, 4, "whatever").pairs,
            PairSelection::GateQubits
        ));
        assert_eq!(options(8, 4, "gate").top_k, 8);
        assert_eq!(options(8, 4, "gate").probs_top, 4);
    }
}
