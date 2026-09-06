// ===== File: lib.rs — TentaQuant quantum front end, IR and simulators =====

//! OpenQASM 3 front end, circuit IR and simulators for TentaQuant.
//!
//! The crate has no async runtime and no I/O: it turns OpenQASM 3 text into a
//! circuit [`ir::Circuit`], simulates it and reports everything the run view
//! draws. The same code compiles to `wasm32` (single-threaded) and to native
//! targets (rayon), so a keyframe computed in the browser and one computed on a
//! node are the same numbers. [`sim::Device`] picks where the state lives: the
//! CPU always, a Vulkan / Metal / DX12 GPU behind the `wgpu` feature.
//!
//! ```
//! use tentaflow_quantum::parse::{parse_qasm3, InputValues};
//! use tentaflow_quantum::sim::statevector::{run, SimOptions};
//! use tentaflow_quantum::sim::{Cancel, Device};
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
//! let result = run(
//!     &circuit,
//!     &SimOptions::default(),
//!     Device::Cpu,
//!     1024,
//!     Cancel::none(),
//! )
//! .unwrap();
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

/// Browser bindings (plan 4.1, tier T0). Behind the `wasm` feature so a native
/// build never links wasm-bindgen; `tentaflow-core/build.rs` turns it on when it
/// generates `www/js/quantum/quantum_glue.*`.
#[cfg(feature = "wasm")]
pub mod wasm;

pub use error::{Error, Result, SourcePos};
pub use gate::Gate;
pub use ir::Circuit;
