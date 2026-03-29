package ai.tentaflow.mobile

/**
 * JNI bridge do biblioteki Rust tentaflow_mobile.
 * Laduje natywna biblioteke i udostepnia funkcje FFI.
 */
object NativeLib {
    init {
        System.loadLibrary("tentaflow_mobile")
    }

    /** Uruchamia Rust core — Router, API server, mesh, inference */
    external fun start()

    /** Powiadomienie o przejsciu aplikacji w tlo */
    external fun onPause()

    /** Powiadomienie o powrocie aplikacji na pierwszy plan */
    external fun onResume()

    /** Powiadomienie o niskiej pamieci */
    external fun onMemoryWarning()
}
