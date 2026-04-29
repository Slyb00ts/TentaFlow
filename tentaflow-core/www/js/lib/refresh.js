// =============================================================================
// Plik: lib/refresh.js
// Opis: Helper do auto-refresh ekranow ze swiadomoscia widocznosci karty.
//       Zamiast goly setInterval — pauzujemy gdy `document.hidden` (zakladka
//       w tle / zminimalizowane okno), bo CPU/sie traci na renderowanie do
//       oka ktore tego nie widzi. Po powrocie do widoku robimy `run()` raz
//       od razu, nie czekajac na nastepny tick.
// =============================================================================

/// Tworzy refresher z pauza na `document.hidden`.
///
/// opts:
///   run            — async () => void; wywolywane co tick + jednorazowo po visibility return
///   intervalMs     — co ile ms gdy strona widoczna (wymagane)
///   hiddenIntervalMs — co ile ms gdy `document.hidden`. `null`/`undefined` = pauza calkowita.
///   immediate      — czy wywolac `run()` od razu po `start()` (default false)
///
/// Zwraca: { start, stop, dispose }
///   - start() — uruchamia loop (idempotentne)
///   - stop()  — zatrzymuje loop ale zostawia listener visibility (pozwala restart)
///   - dispose() — full cleanup: stop + odpiecie visibilitychange
export function createRefresher({ run, intervalMs, hiddenIntervalMs = null, immediate = false }) {
  if (typeof run !== 'function') throw new Error('createRefresher: run must be a function');
  if (!Number.isFinite(intervalMs) || intervalMs <= 0) {
    throw new Error('createRefresher: intervalMs must be positive');
  }

  let timer = null;
  let running = false;
  let busy = false;
  let visibilityBound = false;

  const tick = async () => {
    // Re-entry guard — jesli poprzedni `run` jeszcze trwa (slow API), pomin tick
    // zamiast nakladac wywolania. Wolniejsze odswiezenie > zatkany event loop.
    if (busy) return;
    busy = true;
    try { await run(); }
    catch (_e) { /* error obsluzony przez caller; refresher nie ubija sie */ }
    finally { busy = false; }
  };

  const arm = () => {
    if (timer) { clearInterval(timer); timer = null; }
    if (!running) return;
    if (document.hidden) {
      if (hiddenIntervalMs == null) return; // pauza pelna
      timer = setInterval(tick, hiddenIntervalMs);
    } else {
      timer = setInterval(tick, intervalMs);
    }
  };

  const onVisibilityChange = () => {
    if (!running) return;
    arm();
    // Po powrocie z tla pierwszy refresh natychmiast — uzytkownik nie czeka
    // pelnego intervalu na swiezy widok.
    if (!document.hidden) tick();
  };

  return {
    start() {
      if (running) return;
      running = true;
      if (!visibilityBound) {
        document.addEventListener('visibilitychange', onVisibilityChange);
        visibilityBound = true;
      }
      arm();
      if (immediate) tick();
    },
    stop() {
      running = false;
      if (timer) { clearInterval(timer); timer = null; }
    },
    dispose() {
      this.stop();
      if (visibilityBound) {
        document.removeEventListener('visibilitychange', onVisibilityChange);
        visibilityBound = false;
      }
    },
  };
}
