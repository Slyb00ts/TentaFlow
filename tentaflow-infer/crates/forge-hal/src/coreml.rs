// =============================================================================
// Plik: coreml.rs
// Opis: Wiązanie CoreML (Apple Neural Engine) nad shimem forge_coreml_shim.m.
//       Model to skompilowany katalog .mlmodelc z jednym wejściem i jednym
//       wyjściem MultiArray f16 [rows, cols]; predict pracuje na buforach
//       wołającego bez kopii (initWithDataPointer + outputBackings).
// Przykład:
//       let model = CoreMlModel::load(path, None, ComputeUnits::CpuAndNeuralEngine)?;
//       let shape = model.shape("x", "y")?;
//       let in_place = unsafe { model.predict("x", x_ptr, "y", y_ptr)? };
// =============================================================================

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr::NonNull;

use forge_types::{ForgeError, Result};

#[link(name = "forge_coreml_shim", kind = "static")]
extern "C" {
    fn fc_available() -> c_int;
    fn fc_model_load(
        mlmodelc_path: *const c_char,
        function_name: *const c_char,
        compute_units: c_int,
        err: *mut c_char,
        err_len: c_int,
    ) -> *mut c_void;
    fn fc_model_shape(
        model: *mut c_void,
        in_name: *const c_char,
        in_rows: *mut i64,
        in_cols: *mut i64,
        out_name: *const c_char,
        out_rows: *mut i64,
        out_cols: *mut i64,
    ) -> c_int;
    fn fc_model_predict(
        model: *mut c_void,
        in_name: *const c_char,
        in_ptr: *const c_void,
        out_name: *const c_char,
        out_ptr: *mut c_void,
        err: *mut c_char,
        err_len: c_int,
    ) -> c_int;
    fn fc_model_predict_strided(
        model: *mut c_void,
        in_name: *const c_char,
        in_ptr: *const c_void,
        out_name: *const c_char,
        out_ptr: *mut c_void,
        out_row_stride_elems: i64,
        err: *mut c_char,
        err_len: c_int,
    ) -> c_int;
    fn fc_model_release(model: *mut c_void);
}

const ERR_LEN: usize = 1024;

fn take_error(buf: &[c_char; ERR_LEN]) -> String {
    // Bezpieczne: shim zawsze wpisuje do bufora ciąg zakończony NUL (lub
    // zostawia bufor wyzerowany, gdy nie ma nic do powiedzenia).
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn c_name(name: &str, what: &str) -> Result<CString> {
    CString::new(name).map_err(|_| ForgeError::Other(format!("CoreML: {what} zawiera bajt zerowy")))
}

/// Czy runtime CoreML jest dostępny na tej maszynie.
pub fn is_available() -> bool {
    unsafe { fc_available() != 0 }
}

/// Jednostki obliczeniowe, na które CoreML może rozłożyć model. Wartości
/// odpowiadają wprost `MLComputeUnits` i kodowi przyjmowanemu przez shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeUnits {
    /// CPU + Neural Engine; bez GPU. Domyślna dla pomiarów ANE, bo CoreML
    /// nie ma wtedy pokusy przeniesienia części grafu na Metal.
    CpuAndNeuralEngine,
    /// Wszystkie jednostki, wybór zostawiony planerowi CoreML.
    All,
    /// Tylko CPU — punkt odniesienia.
    CpuOnly,
    /// CPU + GPU, bez ANE.
    CpuAndGpu,
}

impl ComputeUnits {
    fn code(self) -> c_int {
        match self {
            Self::CpuAndNeuralEngine => 0,
            Self::All => 1,
            Self::CpuOnly => 2,
            Self::CpuAndGpu => 3,
        }
    }
}

/// Kształty 2D wejścia i wyjścia modelu, odczytane z jego opisu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoShape {
    pub in_rows: u32,
    pub in_cols: u32,
    pub out_rows: u32,
    pub out_cols: u32,
}

impl IoShape {
    /// Liczba bajtów ciągłego bufora wejściowego f16.
    pub fn in_bytes(&self) -> usize {
        self.in_rows as usize * self.in_cols as usize * 2
    }

    /// Liczba bajtów ciągłego bufora wyjściowego f16.
    pub fn out_bytes(&self) -> usize {
        self.out_rows as usize * self.out_cols as usize * 2
    }
}

/// Załadowany model CoreML. Uchwyt jest zatrzymanym (ARC) obiektem `MLModel`.
pub struct CoreMlModel(NonNull<c_void>);

// SAFETY: Apple dokumentuje `MLModel` jako bezpieczny wielowątkowo — jeden
// obiekt może obsługiwać `prediction` z wielu wątków naraz, a ładowanie
// i zwalnianie przez ARC nie zależy od wątku, który je wykonuje. Uchwyt nie
// trzyma żadnego stanu poza wskaźnikiem, więc przeniesienie go między
// wątkami (Send) i współdzielenie przez `&self` (Sync) nie łamie niczego,
// czego CoreML sam nie gwarantuje.
unsafe impl Send for CoreMlModel {}
unsafe impl Sync for CoreMlModel {}

impl Drop for CoreMlModel {
    fn drop(&mut self) {
        unsafe { fc_model_release(self.0.as_ptr()) };
    }
}

impl std::fmt::Debug for CoreMlModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CoreMlModel(MLModel @ {:p})", self.0.as_ptr())
    }
}

impl CoreMlModel {
    /// Ładuje skompilowany model (`.mlmodelc`). `function` wybiera funkcję
    /// modelu wielofunkcyjnego (macOS 15+); `None` bierze domyślną.
    pub fn load(path: &Path, function: Option<&str>, units: ComputeUnits) -> Result<Self> {
        let path_c = c_name(&path.to_string_lossy(), "ścieżka modelu")?;
        let function_c = match function {
            Some(name) => Some(c_name(name, "nazwa funkcji")?),
            None => None,
        };
        let mut err = [0 as c_char; ERR_LEN];
        let raw = unsafe {
            fc_model_load(
                path_c.as_ptr(),
                function_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                units.code(),
                err.as_mut_ptr(),
                ERR_LEN as c_int,
            )
        };
        NonNull::new(raw).map(Self).ok_or_else(|| {
            ForgeError::Other(format!(
                "CoreML: ładowanie modelu {}: {}",
                path.display(),
                take_error(&err)
            ))
        })
    }

    /// Kształty 2D wejścia `in_name` i wyjścia `out_name` z opisu modelu.
    pub fn shape(&self, in_name: &str, out_name: &str) -> Result<IoShape> {
        let in_c = c_name(in_name, "nazwa wejścia")?;
        let out_c = c_name(out_name, "nazwa wyjścia")?;
        let (mut ir, mut ic, mut or, mut oc) = (0i64, 0i64, 0i64, 0i64);
        let rc = unsafe {
            fc_model_shape(
                self.0.as_ptr(),
                in_c.as_ptr(),
                &mut ir,
                &mut ic,
                out_c.as_ptr(),
                &mut or,
                &mut oc,
            )
        };
        if rc != 0 {
            return Err(ForgeError::Other(format!(
                "CoreML: kształt ('{in_name}' -> '{out_name}'): {}",
                describe_code(rc)
            )));
        }
        let to_u32 = |v: i64, what: &str| -> Result<u32> {
            u32::try_from(v).map_err(|_| {
                ForgeError::Other(format!("CoreML: wymiar {what} = {v} poza zakresem u32"))
            })
        };
        Ok(IoShape {
            in_rows: to_u32(ir, "in_rows")?,
            in_cols: to_u32(ic, "in_cols")?,
            out_rows: to_u32(or, "out_rows")?,
            out_cols: to_u32(oc, "out_cols")?,
        })
    }

    /// Jedno wywołanie predict na ciągłych buforach f16.
    ///
    /// Zwraca `Ok(true)`, gdy CoreML zapisał wynik bezpośrednio w `out`
    /// (ścieżka zero-copy), `Ok(false)`, gdy odrzucił podstawiony bufor i shim
    /// musiał skopiować wynik — poprawność jest zachowana, ale wołający
    /// powinien to zalogować, bo kosztuje to przepustowość pamięci.
    ///
    /// # Safety
    /// * `x` wskazuje na co najmniej `in_rows * in_cols * 2` bajtów ważnych
    ///   przez cały czas trwania wywołania i niezmienianych w tym czasie;
    /// * `out` wskazuje na co najmniej `out_rows * out_cols * 2` bajtów, do
    ///   których nikt inny nie pisze ani z nich nie czyta w trakcie wywołania;
    /// * `x` i `out` nie nakładają się;
    /// * oba wskaźniki są wyrównane do 2 bajtów (f16). Dla ścieżki ANE bez
    ///   kopii w praktyce potrzebne jest wyrównanie do strony — bufory Metala
    ///   `Shared` je mają.
    pub unsafe fn predict(
        &self,
        in_name: &str,
        x: *const u8,
        out_name: &str,
        out: *mut u8,
    ) -> Result<bool> {
        let in_c = c_name(in_name, "nazwa wejścia")?;
        let out_c = c_name(out_name, "nazwa wyjścia")?;
        let mut err = [0 as c_char; ERR_LEN];
        let rc = fc_model_predict(
            self.0.as_ptr(),
            in_c.as_ptr(),
            x as *const c_void,
            out_c.as_ptr(),
            out as *mut c_void,
            err.as_mut_ptr(),
            ERR_LEN as c_int,
        );
        Self::interpret(rc, &err, "predict")
    }

    /// Jak [`predict`](Self::predict), ale wiersze wyjścia leżą co
    /// `out_row_stride_elems` elementów f16 (>= `out_cols`), co pozwala pisać
    /// wprost w kolumnowy wycinek większej macierzy.
    ///
    /// # Safety
    /// Jak w [`predict`](Self::predict), z tą różnicą, że `out` musi obejmować
    /// `((out_rows - 1) * out_row_stride_elems + out_cols) * 2` bajtów.
    pub unsafe fn predict_strided(
        &self,
        in_name: &str,
        x: *const u8,
        out_name: &str,
        out: *mut u8,
        out_row_stride_elems: u32,
    ) -> Result<bool> {
        let in_c = c_name(in_name, "nazwa wejścia")?;
        let out_c = c_name(out_name, "nazwa wyjścia")?;
        let mut err = [0 as c_char; ERR_LEN];
        let rc = fc_model_predict_strided(
            self.0.as_ptr(),
            in_c.as_ptr(),
            x as *const c_void,
            out_c.as_ptr(),
            out as *mut c_void,
            i64::from(out_row_stride_elems),
            err.as_mut_ptr(),
            ERR_LEN as c_int,
        );
        Self::interpret(rc, &err, "predict_strided")
    }

    fn interpret(rc: c_int, err: &[c_char; ERR_LEN], what: &str) -> Result<bool> {
        match rc {
            0 => Ok(true),
            1 => {
                tracing::warn!(
                    "CoreML: {what} — outputBackings nie zostało uszanowane, wynik skopiowano"
                );
                Ok(false)
            }
            code => Err(ForgeError::Other(format!(
                "CoreML: {what}: {} ({})",
                take_error(err),
                describe_code(code)
            ))),
        }
    }
}

/// Opis kodu błędu shimu — tłumaczy liczbę na przyczynę, bo shim bez bufora
/// `err` (fc_model_shape) nie ma innego kanału.
fn describe_code(code: c_int) -> &'static str {
    match code {
        -1 => "błędny argument",
        -2 => "brak wejścia o tej nazwie",
        -3 => "wejście nie jest MultiArray 2D",
        -4 => "brak wyjścia o tej nazwie",
        -5 => "wyjście nie jest MultiArray 2D",
        -6 => "nie udało się utworzyć MLMultiArray nad buforem",
        -7 => "nie udało się utworzyć MLDictionaryFeatureProvider",
        -8 => "prediction zwróciło błąd",
        -9 => "wynik nie zawiera oczekiwanego wyjścia",
        -10 => "wynik ma inny typ lub kształt niż deklaracja",
        -11 => "cecha nie jest MultiArray f16",
        -12 => "wyjątek Objective-C w CoreML",
        _ => "nieznany kod",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_available_on_apple() {
        assert!(is_available());
    }

    #[test]
    fn missing_model_reports_path_and_message() {
        let err = CoreMlModel::load(
            Path::new("/nonexistent/forge_probe.mlmodelc"),
            None,
            ComputeUnits::CpuOnly,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("forge_probe.mlmodelc"), "bez ścieżki: {msg}");
    }

    #[test]
    fn compute_unit_codes_match_shim_contract() {
        assert_eq!(ComputeUnits::CpuAndNeuralEngine.code(), 0);
        assert_eq!(ComputeUnits::All.code(), 1);
        assert_eq!(ComputeUnits::CpuOnly.code(), 2);
        assert_eq!(ComputeUnits::CpuAndGpu.code(), 3);
    }
}
