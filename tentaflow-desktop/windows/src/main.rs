#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    tentaflow_desktop_core::run()
}
