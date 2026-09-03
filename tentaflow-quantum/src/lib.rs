// ===== File: lib.rs — TentaQuant quantum front end, IR and simulators =====

//! OpenQASM 3 front end, circuit IR and simulators for TentaQuant.
//!
//! The crate has no async runtime and no I/O: it turns OpenQASM 3 text into a
//! circuit [`ir::Circuit`], simulates it on the CPU and reports everything the
//! run view draws. The same code compiles to `wasm32` (single-threaded) and to
//! native targets (rayon), so a keyframe computed in the browser and one
//! computed on a node are the same numbers.
//!
//! ```
//! use tentaflow_quantum::parse::{parse_qasm3, InputValues};
//! use tentaflow_quantum::sim::statevector::{run, SimOptions};
//!
//! let circuit = parse_qasm3(
//!     "OPENQASM 3.0;\n\
//!      include \"stdgates.inc\";\n\
//!      qubit[2] q;\n\
//!      bit[2] c;\n\
//!      h q[0];\n\
//!      cx q[0], q[1];\n\
//!      c = measure q;\n",
//!     &InputValues::new(),
//! )
//! .unwrap();
//! let result = run(&circuit, &SimOptions::default(), 1024).unwrap();
//! assert_eq!(result.counts.keys().count(), 2);
//! ```

pub mod error;
pub mod export;
pub mod gate;
pub mod grade;
pub mod ir;
pub mod linalg;
pub mod parse;
pub mod sim;

pub use error::{Error, Result, SourcePos};
pub use gate::Gate;
pub use ir::Circuit;
