// =============================================================================
// Plik: MainActivity.kt
// Opis: Glowna aktywnosc Android — docelowo laduje dashboard w WebView.
//       Rust core uruchamiany jest w TentaFlowApplication.onCreate().
//       Lifecycle callbacks delegowane do Rust przez JNI.
// =============================================================================

package ai.tentaflow.mobile

import android.Manifest
import android.annotation.SuppressLint
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.http.SslError
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.webkit.PermissionRequest
import android.webkit.SslErrorHandler
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.Executors

/**
 * Glowna aktywnosc aplikacji TentaFlow.
 * Rust core (serwer HTTPS + mesh + inference) uruchomiony w TentaFlowApplication.
 * UI mobilne jest renderowane przez dashboard w WebView.
 */
class MainActivity : AppCompatActivity() {
    private lateinit var webView: WebView
    private lateinit var loadingView: LinearLayout
    private lateinit var statusText: TextView
    private lateinit var retryText: TextView

    private val mainHandler = Handler(Looper.getMainLooper())
    private val probeExecutor = Executors.newSingleThreadExecutor()
    private var attempts = 0

    private val cameraPermissionLauncher =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) {
            // Depth (ARCore) needs the camera grant; if it lands after sensors started,
            // start depth now instead of waiting for an app restart.
            startDepthIfNeeded()
        }

    // Phone-as-sensor-robot: native capture → core fusion engine + shared map.
    private var sensorBridge: SensorBridge? = null
    private var depthCapture: DepthCapture? = null
    private var cameraEncoder: CameraEncoder? = null

    private val sensorPermissionLauncher =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
            startSensors()
        }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        requestCameraPermission()
        // Ask for location, then start sensor capture (IMU/baro need no runtime perm;
        // GPS does; depth uses the camera grant). The CORE per-sensor permissions of
        // the `phone` addon gate what actually reaches the fusion engine.
        sensorPermissionLauncher.launch(
            arrayOf(
                Manifest.permission.ACCESS_FINE_LOCATION,
                Manifest.permission.ACCESS_COARSE_LOCATION,
            )
        )

        val root = FrameLayout(this).apply {
            setBackgroundColor(Color.BLACK)
            layoutParams = FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
        }

        webView = WebView(this).apply {
            setBackgroundColor(Color.BLACK)
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.databaseEnabled = true
            settings.mediaPlaybackRequiresUserGesture = false
            settings.cacheMode = WebSettings.LOAD_NO_CACHE
            settings.mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
            webViewClient = TentaFlowWebViewClient()
            webChromeClient = TentaFlowWebChromeClient()
        }

        loadingView = buildLoadingView()
        root.addView(webView)
        root.addView(loadingView)
        setContentView(root)

        pollServer()
    }

    override fun onPause() {
        super.onPause()
        NativeLib.onPause()
    }

    override fun onResume() {
        super.onResume()
        NativeLib.onResume()
        if (::webView.isInitialized) {
            pollServer(reloadOnly = true)
        }
    }

    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        if (level >= TRIM_MEMORY_RUNNING_LOW) {
            NativeLib.onMemoryWarning()
        }
    }

    override fun onDestroy() {
        cameraEncoder?.stop()
        depthCapture?.stop()
        sensorBridge?.stop()
        if (::webView.isInitialized) {
            webView.destroy()
        }
        probeExecutor.shutdownNow()
        super.onDestroy()
    }

    /** Start native sensor capture (idempotent). GPS only if location was granted;
     *  ARCore depth only on supported devices. Core gates each by addon permission. */
    private fun startSensors() {
        if (sensorBridge != null) return
        val gnss = ContextCompat.checkSelfPermission(this, Manifest.permission.ACCESS_FINE_LOCATION) ==
            PackageManager.PERMISSION_GRANTED
        val bridge = SensorBridge(applicationContext)
        bridge.start(imu = true, baro = true, gnss = gnss)
        sensorBridge = bridge
        startDepthIfNeeded()
    }

    /** Start ARCore depth once the camera grant + an ARCore-capable device are present.
     *  Idempotent; safe to call from both the sensor start and the camera-grant callback. */
    private fun startDepthIfNeeded() {
        val bridge = sensorBridge ?: return
        if (depthCapture != null) return
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) !=
            PackageManager.PERMISSION_GRANTED
        ) return
        depthCapture = DepthCapture(this, bridge).also {
            if (!it.start()) depthCapture = null
        }
        // The camera is one device: if ARCore depth claimed it, it maps via hardware
        // depth. Otherwise stream the camera (tile + TentaVision + AI-depth path).
        if (depthCapture == null && cameraEncoder == null) {
            cameraEncoder = CameraEncoder(applicationContext).also {
                if (!it.start()) cameraEncoder = null
            }
        }
    }

    private fun requestCameraPermission() {
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA)
            != PackageManager.PERMISSION_GRANTED
        ) {
            cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
        }
    }

    private fun buildLoadingView(): LinearLayout {
        val title = TextView(this).apply {
            text = "TentaFlow"
            setTextColor(Color.WHITE)
            textSize = 22f
            gravity = Gravity.CENTER
        }

        statusText = TextView(this).apply {
            text = "Uruchamianie serwera..."
            setTextColor(Color.rgb(170, 170, 170))
            textSize = 13f
            gravity = Gravity.CENTER
        }

        retryText = TextView(this).apply {
            setTextColor(Color.rgb(110, 110, 110))
            textSize = 11f
            gravity = Gravity.CENTER
        }

        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            setBackgroundColor(Color.BLACK)
            val padding = (24 * resources.displayMetrics.density).toInt()
            setPadding(padding, padding, padding, padding)
            addView(ProgressBar(this@MainActivity))
            addView(title)
            addView(statusText)
            addView(retryText)
            layoutParams = FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
        }
    }

    private fun pollServer(reloadOnly: Boolean = false) {
        attempts += 1
        val attempt = attempts
        statusText.text = when {
            reloadOnly -> "Odnawianie polaczenia..."
            attempt <= 3 -> "Uruchamianie serwera..."
            attempt <= 10 -> "Oczekiwanie na serwer..."
            else -> "Serwer nie odpowiada"
        }
        retryText.text = if (attempt > 1) "Proba $attempt" else ""
        loadingView.visibility = if (reloadOnly) View.GONE else View.VISIBLE

        probeExecutor.execute {
            val ready = isLocalServerPortOpen()
            mainHandler.post {
                if (ready) {
                    loadDashboard(reloadOnly)
                } else {
                    mainHandler.postDelayed({ pollServer(reloadOnly) }, SERVER_RETRY_DELAY_MS)
                }
            }
        }
    }

    private fun isLocalServerPortOpen(): Boolean {
        return try {
            Socket().use { socket ->
                socket.connect(InetSocketAddress(LOCALHOST, SERVER_PORT), SERVER_CONNECT_TIMEOUT_MS)
                true
            }
        } catch (_: Exception) {
            false
        }
    }

    private fun loadDashboard(reloadOnly: Boolean) {
        if (reloadOnly && webView.url == DASHBOARD_URL) {
            webView.reload()
        } else {
            webView.loadUrl(DASHBOARD_URL)
        }
    }

    private inner class TentaFlowWebViewClient : WebViewClient() {
        override fun onPageFinished(view: WebView, url: String) {
            loadingView.visibility = View.GONE
        }

        override fun onReceivedSslError(
            view: WebView,
            handler: SslErrorHandler,
            error: SslError
        ) {
            if (error.url.startsWith(DASHBOARD_URL)) {
                handler.proceed()
            } else {
                handler.cancel()
            }
        }

        override fun onReceivedError(
            view: WebView,
            request: WebResourceRequest,
            error: WebResourceError
        ) {
            if (request.isForMainFrame) {
                loadingView.visibility = View.VISIBLE
                mainHandler.postDelayed({ pollServer() }, SERVER_RETRY_DELAY_MS)
            }
        }
    }

    private class TentaFlowWebChromeClient : WebChromeClient() {
        override fun onPermissionRequest(request: PermissionRequest) {
            val allowed = request.resources.filter {
                it == PermissionRequest.RESOURCE_VIDEO_CAPTURE ||
                    it == PermissionRequest.RESOURCE_AUDIO_CAPTURE
            }.toTypedArray()

            if (allowed.isNotEmpty()) {
                request.grant(allowed)
            } else {
                request.deny()
            }
        }
    }

    companion object {
        private const val LOCALHOST = "127.0.0.1"
        private const val SERVER_PORT = 8090
        private const val DASHBOARD_URL = "https://127.0.0.1:8090/"
        private const val SERVER_CONNECT_TIMEOUT_MS = 600
        private const val SERVER_RETRY_DELAY_MS = 1500L
    }
}
