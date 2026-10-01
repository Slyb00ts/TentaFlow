//! The code a unit or a position goes by in the file. A stored code is used as
//! it is; one without a code (created in the editor) is addressed by a code
//! derived from its id, so an export of any structure can be imported again.

use crate::services::org_structure::types::{Position, Unit};

fn derived(prefix: &str, id: &str) -> String {
    let short: String = id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(8)
        .collect::<String>()
        .to_lowercase();
    format!("{prefix}-{short}")
}

pub fn unit_code(unit: &Unit) -> String {
    unit.code
        .clone()
        .unwrap_or_else(|| derived("U", &unit.unit_id))
}

pub fn position_code(position: &Position) -> String {
    position
        .code
        .clone()
        .unwrap_or_else(|| derived("P", &position.position_id))
}

/// The lookup form of a code: the file is edited in a spreadsheet, which
/// does not keep the case.
pub fn key(code: &str) -> String {
    code.to_lowercase()
}

/// The derived code of a unit or position that has none; `None` when it has one.
pub fn derived_unit_key(unit: &Unit) -> Option<String> {
    unit.code
        .is_none()
        .then(|| key(&derived("U", &unit.unit_id)))
}

pub fn derived_position_key(position: &Position) -> Option<String> {
    position
        .code
        .is_none()
        .then(|| key(&derived("P", &position.position_id)))
}
