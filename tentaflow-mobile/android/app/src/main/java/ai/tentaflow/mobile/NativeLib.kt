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
    external fun start(dataDir: String)

    /** Powiadomienie o przejsciu aplikacji w tlo */
    external fun onPause()

    /** Powiadomienie o powrocie aplikacji na pierwszy plan */
    external fun onResume()

    /** Powiadomienie o niskiej pamieci */
    external fun onMemoryWarning()

    /**
     * Wpycha jedną zakodowaną KANONICZNĄ próbkę czujnika do hostowej kolejki; addon
     * `phone` opróżnia ją co tick do silnika fuzji (ESKF) + wspólnej mapy.
     * @param kind 1=IMU, 2=GNSS, 3=BARO, 4=DEPTH(LidarFrame) — zgodne ze SENSOR_KIND_*.
     * @param data kanoniczne bajty little-endian (layout z tentaflow-sdk-spec).
     */
    external fun pushSensor(kind: Int, data: ByteArray): Boolean

    /** Czyści bufor czujników (rozłączenie / pauza aplikacji). */
    external fun clearSensors()
}
