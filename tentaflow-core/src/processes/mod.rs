// ============ File: processes/mod.rs — BPMN B1 process validation, persistence, XML, and execution ============

pub mod bpmn;
pub mod calendar;
pub mod jobs;
pub mod model;
pub mod messages;
pub mod repository;
pub mod runtime;
pub mod simulation;
pub mod simulation_schema;
pub mod timers;

#[cfg(test)]
mod activity_result_tests;
#[cfg(test)]
mod activity_io_tests;

#[cfg(test)]
mod scope_tests;

#[cfg(test)]
mod call_pin_tests;

#[cfg(test)]
mod call_tests;

#[cfg(test)]
mod call_producer_tests;

#[cfg(test)]
mod repetition_tests;
#[cfg(test)]
mod repetition_service_tests;
#[cfg(test)]
mod repetition_loop_tests;

#[cfg(test)]
mod repetition_projection_tests;
#[cfg(test)]
mod repetition_capacity_tests;
#[cfg(test)]
mod script_tests;
#[cfg(test)]
mod script_proof_tests;
#[cfg(test)]
mod manual_tests;
#[cfg(test)]
mod manual_proof_tests;
#[cfg(test)]
mod send_receive_tests;
#[cfg(test)]
mod send_receive_proof_tests;
#[cfg(test)]
mod signal_tests;
#[cfg(test)]
mod signal_proof_tests;
#[cfg(test)]
mod send_boundary_tests;
#[cfg(test)]
mod send_boundary_proof_tests;
#[cfg(test)]
mod repetition_boundary_tests;

#[cfg(test)]
mod simulation_tests;
