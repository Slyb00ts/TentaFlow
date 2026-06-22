package ai.tentaflow.mobile

import android.app.Activity
import android.media.Image
import android.os.SystemClock
import com.google.ar.core.Config
import com.google.ar.core.Frame
import com.google.ar.core.Session
import com.google.ar.core.exceptions.NotYetAvailableException

/**
 * Przechwyt głębi z ARCore → chmura punktów w układzie ŚWIATA → kanoniczny
 * LidarFrame (przez [SensorBridge.pushDepth]). Rzutujemy obraz głębi przez
 * intrinsics kamery na punkty w układzie kamery, transformujemy pozą kamery do
 * świata (ARCore world frame) i pakujemy. Rdzeń traktuje to jak każdą ramkę
 * LiDAR (world-frame, jak Go2); pozycję globalną (WGS84) daje osobno ESKF.
 *
 * UWAGA (do dostrojenia na urządzeniu): układ świata ARCore i układ ENU silnika
 * ESKF to dwa lokalne układy metryczne. Mapa buduje się w układzie ARCore;
 * georeferencja (przypięcie do WGS84) następuje przez auto-geo-anchor z GNSS po
 * stronie sceny — wyrównanie obu układów to krok integracyjny na żywym sprzęcie.
 *
 * Wymaga ARCore Depth API (urządzenie wspierające). Bez głębi telefon i tak
 * pozycjonuje (IMU+GNSS) i może udostępniać kamerę.
 */
class DepthCapture(
    private val activity: Activity,
    private val sensorBridge: SensorBridge,
) {
    private var session: Session? = null
    private var thread: Thread? = null
    @Volatile private var running = false
    private var seq = 0

    // Co który piksel głębi próbkujemy (gęstość vs. koszt). 8 → ~co 8. piksel.
    private val stride = 8
    // Rozdzielczość woksela mapy (m) — zgłaszana w nagłówku ramki.
    private val resolution = 0.05f
    private val maxDepthM = 8.0f

    fun start(): Boolean {
        val s = try {
            Session(activity).also {
                val cfg = Config(it)
                if (it.isDepthModeSupported(Config.DepthMode.AUTOMATIC)) {
                    cfg.depthMode = Config.DepthMode.AUTOMATIC
                } else {
                    return false // brak wsparcia głębi — pomijamy ścieżkę depth
                }
                cfg.focusMode = Config.FocusMode.AUTO
                it.configure(cfg)
            }
        } catch (e: Exception) {
            return false
        }
        session = s
        s.resume()
        running = true
        thread = Thread { loop() }.also { it.start() }
        return true
    }

    fun stop() {
        running = false
        thread?.join(500)
        thread = null
        runCatching { session?.pause() }
        runCatching { session?.close() }
        session = null
    }

    private fun loop() {
        val s = session ?: return
        while (running) {
            val frame = try {
                s.update()
            } catch (e: Exception) {
                continue
            }
            processFrame(frame)
            // ~5–10 Hz to wystarczy dla mapy; oszczędza CPU/baterię.
            SystemClock.sleep(120)
        }
    }

    private fun processFrame(frame: Frame) {
        val camera = frame.camera
        if (camera.trackingState != com.google.ar.core.TrackingState.TRACKING) return
        // AR device pose (ARCore world frame, Y-up) → engine Z-up: pos [x,-z,y],
        // orientation rotated +90° about X. Drives marker + map frame + AR↔ENU align.
        val cp = camera.pose
        // q_zup = q_conv * q_ar, q_conv = (sin45,0,0,cos45) about +X.
        val s = 0.70710677f
        val qx = cp.qx(); val qy = cp.qy(); val qz = cp.qz(); val qw = cp.qw()
        sensorBridge.pushDevicePose(
            SystemClock.elapsedRealtimeNanos() / 1000,
            floatArrayOf(cp.tx(), -cp.tz(), cp.ty()),
            floatArrayOf(
                s * (qx + qw),   // x
                s * (qy - qz),   // y
                s * (qy + qz),   // z
                s * (qw - qx),   // w
            ),
        )
        val depth: Image = try {
            frame.acquireDepthImage16Bits()
        } catch (e: NotYetAvailableException) {
            return
        } catch (e: Exception) {
            return
        }
        try {
            val w = depth.width
            val h = depth.height
            val plane = depth.planes[0]
            val bufStride = plane.rowStride
            val pixStride = plane.pixelStride
            val data = plane.buffer

            // Intrinsics dopasowane do rozdzielczości obrazu głębi.
            val intr = camera.textureIntrinsics
            val fx = intr.focalLength[0] * w / intr.imageDimensions[0]
            val fy = intr.focalLength[1] * h / intr.imageDimensions[1]
            val cx = intr.principalPoint[0] * w / intr.imageDimensions[0]
            val cy = intr.principalPoint[1] * h / intr.imageDimensions[1]

            // Poza kamery (camera → world), kolumnowa macierz 4×4.
            val pose = FloatArray(16)
            camera.pose.toMatrix(pose, 0)

            val out = ArrayList<Float>(w * h / (stride * stride) * 3)
            var v = 0
            while (v < h) {
                var u = 0
                while (u < w) {
                    val idx = v * bufStride + u * pixStride
                    val mm = (data.get(idx).toInt() and 0xFF) or
                        ((data.get(idx + 1).toInt() and 0xFF) shl 8)
                    if (mm != 0) {
                        val z = mm / 1000f
                        if (z in 0.1f..maxDepthM) {
                            // Punkt w układzie kamery (ARCore: +X right, +Y up, patrzy w −Z).
                            val xc = (u - cx) * z / fx
                            val yc = -(v - cy) * z / fy
                            val zc = -z
                            // world = pose * [xc,yc,zc,1] (macierz kolumnowa).
                            val xw = pose[0] * xc + pose[4] * yc + pose[8] * zc + pose[12]
                            val yw = pose[1] * xc + pose[5] * yc + pose[9] * zc + pose[13]
                            val zw = pose[2] * xc + pose[6] * yc + pose[10] * zc + pose[14]
                            // Y-up (ARCore) → engine Z-up: [x, -z, y].
                            out.add(xw); out.add(-zw); out.add(yw)
                        }
                    }
                    u += stride
                }
                v += stride
            }
            if (out.isNotEmpty()) {
                val tsUs = SystemClock.elapsedRealtimeNanos() / 1000
                sensorBridge.pushDepth(tsUs, seq++, resolution, out.toFloatArray())
            }
        } finally {
            depth.close()
        }
    }
}
