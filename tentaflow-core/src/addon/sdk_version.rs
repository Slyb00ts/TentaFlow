// =============================================================================
// Plik: addon/sdk_version.rs
// Opis: Sprawdzanie kompatybilnosci wersji SDK addona z rdzeniem TentaFlow.
//       Addony moga deklarowac `addon.sdk_version = ">=0.2.0"` (semver req).
//       Jesli pole jest puste — kompatybilnosc zakladana (addon nie deklaruje
//       wymagan). Wywolywane przez lifecycle.rs przed zaladowaniem modulu WASM.
// =============================================================================

use semver::{Version, VersionReq};
use thiserror::Error;

/// Aktualna wersja SDK rdzenia. Bumpowac przy kazdej zmianie ABI lamiacej
/// kompatybilnosc (dodanie pola w manifescie nie lamie; usuniecie host function
/// lamie). Manifest addona moze wyrazic `sdk_version = ">=0.2.0, <1.0"` zeby
/// rdzen odrzucil instalacje gdy nie pasuje.
pub const CORE_SDK_VERSION: &str = "0.2.0";

/// Dedykowany typ bledu dla check_compatibility — rozroznia inwalidny semver
/// od mismatchu wymagan vs rdzen. Wczesniej oba przypadki zwracaly
/// AbiError::Operation (kod 5) co utrudnialo diagnoze problemu w lifecycle.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SdkVersionError {
    #[error("Niepoprawny format semver: '{0}'")]
    InvalidSemver(String),

    #[error("Manifest wymaga SDK '{required}', rdzen ma '{core}'")]
    Incompatible { required: String, core: String },
}

/// Parsuje deklaracje wymagan SDK addona jako `VersionReq`.
pub fn parse_addon_sdk_version_req(req_str: &str) -> Result<VersionReq, semver::Error> {
    VersionReq::parse(req_str)
}

/// Sprawdza czy zadeklarowana wersja SDK addona jest kompatybilna z rdzeniem.
///
/// - `None` → Ok (addon nie deklaruje wymagan; zakladamy kompatybilnosc).
/// - `Some(req)` → parsuje jako semver VersionReq i sprawdza dopasowanie do
///   CORE_SDK_VERSION. Bledny semver → `InvalidSemver`; mismatch → `Incompatible`.
pub fn check_compatibility(addon_req: Option<&str>) -> Result<(), SdkVersionError> {
    let Some(req_str) = addon_req else {
        return Ok(());
    };

    let req = parse_addon_sdk_version_req(req_str)
        .map_err(|_| SdkVersionError::InvalidSemver(req_str.to_string()))?;

    let core_ver = Version::parse(CORE_SDK_VERSION)
        .expect("CORE_SDK_VERSION musi byc poprawnym semver - stala kompilacji");

    if req.matches(&core_ver) {
        Ok(())
    } else {
        Err(SdkVersionError::Incompatible {
            required: req_str.to_string(),
            core: CORE_SDK_VERSION.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_none_ok() {
        assert!(check_compatibility(None).is_ok());
    }

    #[test]
    fn compat_exact_match_ok() {
        assert!(check_compatibility(Some("0.2.0")).is_ok());
    }

    #[test]
    fn compat_range_match_ok() {
        assert!(check_compatibility(Some(">=0.1.0, <1.0")).is_ok());
        assert!(check_compatibility(Some(">=0.2")).is_ok());
        assert!(check_compatibility(Some("^0.2")).is_ok());
    }

    #[test]
    fn compat_range_mismatch_returns_incompatible() {
        let err = check_compatibility(Some(">=99.0")).unwrap_err();
        match err {
            SdkVersionError::Incompatible { required, core } => {
                assert_eq!(required, ">=99.0");
                assert_eq!(core, CORE_SDK_VERSION);
            }
            other => panic!("oczekiwano Incompatible, dostalem {other:?}"),
        }
    }

    #[test]
    fn compat_too_old_returns_incompatible() {
        let err = check_compatibility(Some("<0.1.0")).unwrap_err();
        assert!(matches!(err, SdkVersionError::Incompatible { .. }));
    }

    #[test]
    fn compat_invalid_semver_returns_invalid_semver() {
        let err = check_compatibility(Some("not semver at all")).unwrap_err();
        match err {
            SdkVersionError::InvalidSemver(s) => assert_eq!(s, "not semver at all"),
            other => panic!("oczekiwano InvalidSemver, dostalem {other:?}"),
        }
    }

    #[test]
    fn core_sdk_version_parseable() {
        // Stala musi byc parsowalna w runtime — chroni przed regresja.
        Version::parse(CORE_SDK_VERSION).expect("CORE_SDK_VERSION poprawny semver");
    }
}
