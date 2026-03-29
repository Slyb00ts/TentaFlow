// =============================================================================
// Plik: MainActivity.kt
// Opis: Glowna aktywnosc — egui renderuje na Vulkan/OpenGL surface (wgpu).
//       Rust core uruchomiony w TentaFlowApplication.onCreate().
//       Lifecycle callbacks delegowane do Rust przez JNI.
// =============================================================================

package ai.tentaflow.mobile

import android.os.Bundle
import androidx.appcompat.app.AppCompatActivity

/**
 * Glowna aktywnosc aplikacji TentaFlow.
 * Rust core (serwer HTTPS + mesh + inference) uruchomiony w TentaFlowApplication.
 * GUI egui renderowane na natywnej surface przez wgpu (Vulkan/OpenGL).
 */
class MainActivity : AppCompatActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // egui/wgpu tworzy wlasna surface — nie potrzebuje setContentView
    }

    override fun onPause() {
        super.onPause()
        NativeLib.onPause()
    }

    override fun onResume() {
        super.onResume()
        NativeLib.onResume()
    }

    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        if (level >= TRIM_MEMORY_RUNNING_LOW) {
            NativeLib.onMemoryWarning()
        }
    }
}
