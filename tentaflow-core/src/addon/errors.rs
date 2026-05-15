// =============================================================================
// Plik: addon/errors.rs
// Opis: Ujednolicony enum bledow ABI (24 kody) zwracanych przez host functions
//       wprowadzane w F1a (TentaVision). Konwencja: wartosci dodatnie 1..24,
//       0 = sukces. Istniejace stale ABI_ERR_* w host_functions/mod.rs uzywaja
//       wartosci ujemnych (-1..-6) i sa zachowane dla wstecznej kompatybilnosci
//       (storage_*, http_*, llm_*, ui_* — host functions sprzed F1a). Nowe host
//       functions (SQL, Alias, Camera, Streaming, Recording — M1.W4-W8) MUSZA
//       uzywac AbiError zgodnie z planem v0.5.3 §6.2.Y.
// =============================================================================

/// Kanoniczne kody bledow ABI dla host functions wprowadzanych w F1a.
///
/// Wartosci sa dodatnie (1..24) — odroznione od starych stalych ABI_ERR_*
/// (ujemnych), ktore pozostaja w uzyciu przez pre-F1a host functions.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiError {
    /// Sukces — operacja zakonczona pomyslnie.
    Ok = 0,
    /// Brak wymaganych uprawnien.
    Permission = 1,
    /// Zasob nie znaleziony.
    NotFound = 2,
    /// Brak dostepnego targetu (np. alias bez podpietego modelu/service).
    NoAvailableTarget = 3,
    /// Przekroczono limit czasu operacji.
    Timeout = 4,
    /// Ogolny blad operacji (nieklasyfikowany).
    Operation = 5,
    /// Bufor wyjsciowy za maly — out_len_ptr zawiera wymagany rozmiar.
    OutputBufferTooSmall = 6,
    /// Konflikt stanu (np. duplikat klucza, alias juz istnieje).
    Conflict = 7,
    /// Bledna skladnia SQL (parser sqlite/postgres odrzucil zapytanie).
    SqlSyntax = 8,
    /// Naruszenie constraint SQL (UNIQUE, NOT NULL, FOREIGN KEY, CHECK).
    SqlConstraint = 9,
    /// Brak wyniku SQL (sql_query_one nie zwrocil zadnego wiersza).
    SqlNoResult = 10,
    /// Przekroczono kwote zasobow (storage / rate limit / fuel).
    QuotaExceeded = 11,
    /// Kamera niedostepna (offline, network unreachable).
    CameraUnreachable = 12,
    /// Bledne dane uwierzytelniajace kamery.
    CameraAuthFailed = 13,
    /// Vendor kamery nieobslugiwany.
    CameraVendorUnsupported = 14,
    /// Strumien nie znaleziony.
    StreamNotFound = 15,
    /// Strumien zamkniety przez druga strone.
    StreamClosed = 16,
    /// Backpressure — addon nie nadaza za strumieniem.
    Backpressure = 17,
    /// Nagranie nie znalezione.
    RecordingNotFound = 18,
    /// Nagranie wyczyszczone (retention policy).
    RecordingPurged = 19,
    /// Zadany timestamp poza zakresem ring-buffera.
    RecordingTimeOutOfRing = 20,
    /// Payload przekroczyl limit wielkosci dla danej kategorii API.
    PayloadTooLarge = 21,
    /// Gate (claim-based) niespelniony — operacja zablokowana przez policy.
    GateNotSatisfied = 22,
    /// PickupToken / FrameToken nieprawidlowy lub wygasly.
    FrameTokenInvalid = 23,
    /// Frame zostal wyczyszczony (pickup po terminie).
    FramePurged = 24,
}

impl AbiError {
    /// Zwraca wartosc i32 (do return z host functions).
    #[inline]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Opis kodu po polsku (bez znakow diakrytycznych).
    pub const fn description(self) -> &'static str {
        match self {
            Self::Ok => "Operacja zakonczona pomyslnie",
            Self::Permission => "Brak wymaganych uprawnien",
            Self::NotFound => "Zasob nie znaleziony",
            Self::NoAvailableTarget => "Brak dostepnego targetu dla aliasu",
            Self::Timeout => "Przekroczono limit czasu",
            Self::Operation => "Ogolny blad operacji",
            Self::OutputBufferTooSmall => "Bufor wyjsciowy za maly (out_len_ptr ma wymagany rozmiar)",
            Self::Conflict => "Konflikt stanu (np. duplikat)",
            Self::SqlSyntax => "Bledna skladnia SQL",
            Self::SqlConstraint => "Naruszenie constraint SQL",
            Self::SqlNoResult => "Zapytanie SQL nie zwrocilo wyniku",
            Self::QuotaExceeded => "Przekroczono kwote zasobow",
            Self::CameraUnreachable => "Kamera niedostepna",
            Self::CameraAuthFailed => "Bledne dane uwierzytelniajace kamery",
            Self::CameraVendorUnsupported => "Vendor kamery nieobslugiwany",
            Self::StreamNotFound => "Strumien nie znaleziony",
            Self::StreamClosed => "Strumien zamkniety",
            Self::Backpressure => "Backpressure — addon nie nadaza za strumieniem",
            Self::RecordingNotFound => "Nagranie nie znalezione",
            Self::RecordingPurged => "Nagranie wyczyszczone (retention)",
            Self::RecordingTimeOutOfRing => "Timestamp poza zakresem ring-buffera",
            Self::PayloadTooLarge => "Payload przekroczyl limit wielkosci",
            Self::GateNotSatisfied => "Gate niespelniony — operacja zablokowana",
            Self::FrameTokenInvalid => "PickupToken/FrameToken nieprawidlowy lub wygasly",
            Self::FramePurged => "Frame zostal wyczyszczony",
        }
    }
}

impl From<AbiError> for i32 {
    #[inline]
    fn from(e: AbiError) -> Self {
        e as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lista wszystkich wariantow — uzywana do testow unique values + descriptions.
    const ALL: &[AbiError] = &[
        AbiError::Ok,
        AbiError::Permission,
        AbiError::NotFound,
        AbiError::NoAvailableTarget,
        AbiError::Timeout,
        AbiError::Operation,
        AbiError::OutputBufferTooSmall,
        AbiError::Conflict,
        AbiError::SqlSyntax,
        AbiError::SqlConstraint,
        AbiError::SqlNoResult,
        AbiError::QuotaExceeded,
        AbiError::CameraUnreachable,
        AbiError::CameraAuthFailed,
        AbiError::CameraVendorUnsupported,
        AbiError::StreamNotFound,
        AbiError::StreamClosed,
        AbiError::Backpressure,
        AbiError::RecordingNotFound,
        AbiError::RecordingPurged,
        AbiError::RecordingTimeOutOfRing,
        AbiError::PayloadTooLarge,
        AbiError::GateNotSatisfied,
        AbiError::FrameTokenInvalid,
        AbiError::FramePurged,
    ];

    #[test]
    fn abi_error_codes_unique() {
        let mut seen = std::collections::HashSet::new();
        for e in ALL {
            assert!(seen.insert(e.as_i32()), "Duplicate AbiError code: {}", e.as_i32());
        }
        // 25 wariantow razem z Ok=0.
        assert_eq!(ALL.len(), 25);
    }

    #[test]
    fn abi_error_codes_match_plan_spec() {
        assert_eq!(AbiError::Ok.as_i32(), 0);
        assert_eq!(AbiError::Permission.as_i32(), 1);
        assert_eq!(AbiError::OutputBufferTooSmall.as_i32(), 6);
        assert_eq!(AbiError::PayloadTooLarge.as_i32(), 21);
        assert_eq!(AbiError::FramePurged.as_i32(), 24);
    }

    #[test]
    fn abi_error_descriptions_nonempty() {
        for e in ALL {
            let d = e.description();
            assert!(!d.is_empty(), "AbiError {:?} has empty description", e);
        }
    }

    #[test]
    fn abi_error_into_i32() {
        let v: i32 = AbiError::Conflict.into();
        assert_eq!(v, 7);
    }
}
