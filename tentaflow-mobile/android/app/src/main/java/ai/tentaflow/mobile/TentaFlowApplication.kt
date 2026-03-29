package ai.tentaflow.mobile

import android.app.Application
import android.util.Log

/**
 * Application class — inicjalizuje Rust core przy starcie aplikacji.
 */
class TentaFlowApplication : Application() {

    override fun onCreate() {
        super.onCreate()
        Log.i(TAG, "Inicjalizacja TentaFlow Mobile...")

        Thread {
            try {
                NativeLib.start()
            } catch (e: Exception) {
                Log.e(TAG, "Blad uruchomienia Rust core", e)
            }
        }.start()
    }

    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        if (level >= TRIM_MEMORY_RUNNING_LOW) {
            NativeLib.onMemoryWarning()
        }
    }

    companion object {
        private const val TAG = "TentaFlowAI"
    }
}
