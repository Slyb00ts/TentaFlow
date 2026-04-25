// =============================================================================
// Plik: main.rs
// Opis: Entrypoint TentaFlow Desktop dla macOS. Inicjalizuje Swift MLX bridge
//       (jesli feature mlx-swift-bridge wlaczone) i przekazuje sterowanie do
//       tentaflow_desktop_core::run() ktore zarzadza tray icon, GUI i
//       backendowym serwerem.
// =============================================================================

#[cfg(all(target_os = "macos", feature = "mlx-swift-bridge"))]
mod mlx_swift_init;

fn main() -> anyhow::Result<()> {
    // Bootstrap Swift MLX bridge zanim core wystartuje silniki inferencji.
    // Bledy logujemy, ale nie zatrzymujemy startu — fallback na inne backendy.
    #[cfg(all(target_os = "macos", feature = "mlx-swift-bridge"))]
    {
        if let Err(e) = mlx_swift_init::init() {
            tracing::warn!(
                "[mlx-swift] Bootstrap nieudany — kontynuuje bez Swift MLX: {:#}",
                e
            );
        }
    }

    tentaflow_desktop_core::run()
}
