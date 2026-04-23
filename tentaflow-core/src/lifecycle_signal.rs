// =============================================================================
// Plik: lifecycle_signal.rs
// Opis: Globalny broadcast-channel dla eventow cyklu zycia aplikacji
//       (Resume/Pause). Uzywane glownie na iOS — po wybudzeniu apki z suspendu
//       poszczegolne podsystemy (unified_server, iroh mesh) musza sie
//       odswiezyc: rebindowac TCP listener, force-reconnect peerow itd.
//       Nadawca: tentaflow_on_resume/pause FFI (tentaflow-mobile/core).
//       Odbiorcy: subscribe() z dowolnego miejsca w tentaflow-core.
// =============================================================================

use std::sync::OnceLock;
use tokio::sync::broadcast;

/// Eventy cyklu zycia hostingowej aplikacji.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// Aplikacja wrocila z tla (iOS applicationWillEnterForeground).
    /// Subsystemy powinny wymusic health check / rebind / reconnect.
    Resume,
    /// Aplikacja idzie w tlo (iOS applicationDidEnterBackground).
    /// Subsystemy moga zmniejszyc aktywnosc (np. wstrzymac heartbeat).
    Pause,
}

static CHANNEL: OnceLock<broadcast::Sender<LifecycleEvent>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<LifecycleEvent> {
    CHANNEL.get_or_init(|| {
        // Pojemnosc 16 — eventy sa rzadkie (co sekundy/minuty), 16 wystarczy
        // zeby subscriber opozniony o jedna iteracje nie dostal Lagged.
        let (tx, _rx) = broadcast::channel(16);
        tx
    })
}

/// Subskrybuje kanal lifecycle. Call z tokio task'u.
pub fn subscribe() -> broadcast::Receiver<LifecycleEvent> {
    channel().subscribe()
}

/// Wysyla Resume do wszystkich subscriberow. No-op jesli brak subscriberow.
pub fn broadcast_resume() {
    let _ = channel().send(LifecycleEvent::Resume);
}

/// Wysyla Pause do wszystkich subscriberow. No-op jesli brak subscriberow.
pub fn broadcast_pause() {
    let _ = channel().send(LifecycleEvent::Pause);
}
