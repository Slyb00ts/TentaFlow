// ===== File: exec.rs — the handle that lets a model not hold a buffer =====
//
// A model is an ORDER OF OPERATIONS. Which silicon runs them is a separate
// question, and the moment the two live in one struct the model stops being
// reusable: it holds device buffers, so it is a model FOR that device, so the
// next device needs its own. That is how one architecture came to be written
// twice here — 2822 lines for CUDA and 1096 for Metal, the same layer order in
// both (docs/PRZEGLAD_UKLADU.md).
//
// This is the first cut in the other direction. Weights live in the executor;
// the model carries indices. What remains is the operations themselves, which
// still reach into the executor's buffers directly — until they too are named
// rather than reached for, `forge-model` cannot stop naming `forge-hal`, which
// PLAN_NAPRAWY 5.1 forbids it from doing.

/// A weight the executor uploaded and now owns.
///
/// Opaque on purpose: the model knows which weight plays which role, not what
/// it is made of. Whether it is four bits or six, which group it uses and how
/// its scales are stored are questions for whoever multiplies it — and a model
/// that cannot ask them is a model that cannot be written for one backend by
/// accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightId(pub u32);

/// Bufor roboczy, nazwany tym, co model w nim trzyma.
///
/// Typ zapisu NIE jest tu wymieniony celowo: wynika ze slotu, a nie z decyzji
/// modelu. Aktywacje idące dalej w mnożenia są półprecyzyjne, a te, które
/// wracają do strumienia rezydualnego albo do wyboru tokenu — pojedynczej.
/// Model, który mógłby to ustawić, mógłby to ustawić źle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Strumień rezydualny.
    Hidden,
    /// Wyjście bieżącej normalizacji.
    Norm,
    Query,
    Key,
    Value,
    /// Wyjście uwagi, przed projekcją wyjściową.
    Attn,
    /// Wynik projekcji, dodawany z powrotem do strumienia.
    Proj,
    Gate,
    Up,
    /// Bramka i „up" złączone aktywacją.
    Activated,
    /// Logity ostatniego tokenu kafla.
    Logits,
}
