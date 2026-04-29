// =============================================================================
// Plik: deploy/mod.rs
// Opis: Deploy kontenerow embedowanych w binarce tentaflow. Build.rs pakuje
//       caly katalog tentaflow-containers/ oraz wspolne crate'y Rust potrzebne
//       wybranym Dockerfile jako tar.gz, deploy::extract_to() rozpakowuje to do
//       tmpdir, a deploy::build_image() i run_container() wolaja Docker przez
//       bollard.
// =============================================================================

#[cfg(feature = "docker")]
pub mod docker;

pub mod bundle;
pub mod log_bus;
pub mod process_ctl;
pub mod python_venv;

pub use bundle::extract_to;
pub mod vram_calculator;
