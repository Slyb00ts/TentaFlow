package ai.tentaflow.mobile

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.os.SystemClock
import androidx.core.content.ContextCompat
import com.google.android.gms.location.FusedLocationProviderClient
import com.google.android.gms.location.LocationCallback
import com.google.android.gms.location.LocationRequest
import com.google.android.gms.location.LocationResult
import com.google.android.gms.location.LocationServices
import com.google.android.gms.location.Priority
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Natywne przechwytywanie czujników telefonu → kanoniczne próbki → Rust core.
 *
 * Każdą próbkę kodujemy DOKŁADNIE w binarnym layoutcie z `tentaflow-sdk-spec`
 * (little-endian) i przekazujemy przez [NativeLib.pushSensor]. Logika fuzji
 * (ESKF) i mapy żyje w rdzeniu — tu jest tylko przechwyt + kodowanie.
 *
 * Każdy czujnik startuje TYLKO gdy włączony przez [start] (toggle aplikacji) i gdy
 * system przyznał odpowiednie uprawnienie OS — brak uprawnienia = czujnik nie
 * rusza (graceful degradation, dokładnie jak po stronie uprawnień addonu).
 */
class SensorBridge(private val context: Context) : SensorEventListener {

    companion object {
        // Zgodne ze stałymi SENSOR_KIND_* w rdzeniu (services/mobile_sensors.rs).
        const val KIND_IMU = 1
        const val KIND_GNSS = 2
        const val KIND_BARO = 3
        const val KIND_DEPTH = 4
        const val KIND_POSE = 5

        // Domyślne modele szumu MEMS (1σ). Addon/silnik mogą je później dostroić.
        const val ACCEL_NOISE = 0.02f   // m/s²
        const val GYRO_NOISE = 0.002f   // rad/s
        const val BARO_NOISE = 0.6f     // m
    }

    private val sensorManager = context.getSystemService(Context.SENSOR_SERVICE) as SensorManager
    private val fused: FusedLocationProviderClient =
        LocationServices.getFusedLocationProviderClient(context)

    private var latestGyro = FloatArray(3)
    private var haveGyro = false

    /** Uruchamia przechwyt dla przyznanych czujników. */
    fun start(imu: Boolean, baro: Boolean, gnss: Boolean) {
        if (imu) {
            sensorManager.getDefaultSensor(Sensor.TYPE_GYROSCOPE)?.let {
                sensorManager.registerListener(this, it, SensorManager.SENSOR_DELAY_GAME)
            }
            sensorManager.getDefaultSensor(Sensor.TYPE_ACCELEROMETER)?.let {
                sensorManager.registerListener(this, it, SensorManager.SENSOR_DELAY_GAME)
            }
        }
        if (baro) {
            sensorManager.getDefaultSensor(Sensor.TYPE_PRESSURE)?.let {
                sensorManager.registerListener(this, it, SensorManager.SENSOR_DELAY_NORMAL)
            }
        }
        if (gnss) startGnss()
    }

    fun stop() {
        sensorManager.unregisterListener(this)
        runCatching { fused.removeLocationUpdates(locationCallback) }
        NativeLib.clearSensors()
    }

    // ----- IMU + barometr (SensorManager) -----

    override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) {}

    override fun onSensorChanged(event: SensorEvent) {
        // Monotoniczny zegar µs (spójny dla wszystkich czujników; dt liczy rdzeń).
        val tsUs = SystemClock.elapsedRealtimeNanos() / 1000
        when (event.sensor.type) {
            Sensor.TYPE_GYROSCOPE -> {
                latestGyro[0] = event.values[0]
                latestGyro[1] = event.values[1]
                latestGyro[2] = event.values[2]
                haveGyro = true
            }
            Sensor.TYPE_ACCELEROMETER -> {
                if (!haveGyro) return // czekamy na pierwszy odczyt żyroskopu
                NativeLib.pushSensor(KIND_IMU, encodeImu(tsUs, event.values, latestGyro))
            }
            Sensor.TYPE_PRESSURE -> {
                val pressurePa = event.values[0] * 100f // hPa → Pa
                val relAlt = SensorManager.getAltitude(
                    SensorManager.PRESSURE_STANDARD_ATMOSPHERE, event.values[0]
                )
                NativeLib.pushSensor(KIND_BARO, encodeBaro(tsUs, pressurePa, relAlt))
            }
        }
    }

    // ----- GNSS (FusedLocationProvider) -----

    private val locationCallback = object : LocationCallback() {
        override fun onLocationResult(result: LocationResult) {
            val loc = result.lastLocation ?: return
            val tsUs = loc.elapsedRealtimeNanos / 1000
            NativeLib.pushSensor(KIND_GNSS, encodeGnss(tsUs, loc))
        }
    }

    private fun startGnss() {
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_FINE_LOCATION)
            != PackageManager.PERMISSION_GRANTED
        ) return
        val req = LocationRequest.Builder(Priority.PRIORITY_HIGH_ACCURACY, 1000L).build()
        runCatching { fused.requestLocationUpdates(req, locationCallback, context.mainLooper) }
    }

    // ----- Kanoniczne enkodery (little-endian, layout z tentaflow-sdk-spec) -----

    private fun buf(len: Int) = ByteBuffer.allocate(len).order(ByteOrder.LITTLE_ENDIAN)

    /** ImuSample (44 B). */
    private fun encodeImu(tsUs: Long, accel: FloatArray, gyro: FloatArray): ByteArray {
        val b = buf(44)
        b.put(1).put(0).put(0).put(0)         // version, flags, 2× reserved
        b.putLong(tsUs)
        b.putFloat(accel[0]); b.putFloat(accel[1]); b.putFloat(accel[2])
        b.putFloat(gyro[0]); b.putFloat(gyro[1]); b.putFloat(gyro[2])
        b.putFloat(ACCEL_NOISE); b.putFloat(GYRO_NOISE)
        return b.array()
    }

    /** BaroSample (24 B). */
    private fun encodeBaro(tsUs: Long, pressurePa: Float, relAltM: Float): ByteArray {
        val b = buf(24)
        b.put(1).put(0).put(0).put(0)
        b.putLong(tsUs)
        b.putFloat(pressurePa); b.putFloat(relAltM); b.putFloat(BARO_NOISE)
        return b.array()
    }

    /** GnssFix (60 B). */
    private fun encodeGnss(tsUs: Long, loc: android.location.Location): ByteArray {
        val b = buf(60)
        val hasVel = loc.hasSpeed()
        b.put(1).put(if (hasVel) 1 else 0).put(0).put(0)
        b.putLong(tsUs)
        b.putDouble(loc.latitude); b.putDouble(loc.longitude); b.putDouble(loc.altitude)
        b.putFloat(if (loc.hasAccuracy()) loc.accuracy else 10f)
        b.putFloat(if (loc.hasVerticalAccuracy()) loc.verticalAccuracyMeters else 20f)
        // Prędkość poziomą (m/s, kurs) rozkładamy na ENU; pion 0 jeśli brak.
        if (hasVel) {
            val course = Math.toRadians(loc.bearing.toDouble())
            val speed = loc.speed
            b.putFloat((speed * Math.sin(course)).toFloat()) // East
            b.putFloat((speed * Math.cos(course)).toFloat()) // North
            b.putFloat(0f)                                   // Up
            b.putFloat(if (loc.hasSpeedAccuracy()) loc.speedAccuracyMetersPerSecond else 0.5f)
        } else {
            b.putFloat(0f); b.putFloat(0f); b.putFloat(0f); b.putFloat(0f)
        }
        return b.array()
    }

    /**
     * Koduje ramkę głębi jako kanoniczny LidarFrame (layout XYZ f32, world-frame).
     * Wywoływane przez [DepthCapture] po rzutowaniu obrazu głębi ARCore na chmurę
     * punktów świata (transform przez pozę kamery — robione w DepthCapture, tu tylko
     * pakowanie). `worldXyz` = spłaszczone [x,y,z,x,y,z,...] w metrach.
     */
    /** Publikuje pozę urządzenia (AR world frame): pozycja [x,y,z] + kwaternion
     *  [x,y,z,w] → znacznik + układ mapy + wyrównanie AR↔ENU. PoseSample (40 B). */
    fun pushDevicePose(tsUs: Long, pos: FloatArray, quat: FloatArray) {
        val b = buf(40)
        b.put(1).put(0).put(0).put(0)
        b.putLong(tsUs)
        b.putFloat(pos[0]); b.putFloat(pos[1]); b.putFloat(pos[2])
        b.putFloat(quat[0]); b.putFloat(quat[1]); b.putFloat(quat[2]); b.putFloat(quat[3])
        NativeLib.pushSensor(KIND_POSE, b.array())
    }

    fun pushDepth(tsUs: Long, frameSeq: Int, resolution: Float, worldXyz: FloatArray) {
        val n = worldXyz.size / 3
        val b = buf(44 + n * 12)
        b.put(2).put(3).put(0).put(0)         // LIDAR_FRAME_VERSION=2, layout XYZ=3
        b.putInt(n)                            // point_count
        b.putInt(frameSeq)
        b.putLong(tsUs)
        b.putLong(0)                           // host_send_us (host stempluje)
        b.putFloat(resolution)
        b.putFloat(0f); b.putFloat(0f); b.putFloat(0f) // origin (punkty już zawierają)
        for (v in worldXyz) b.putFloat(v)
        NativeLib.pushSensor(KIND_DEPTH, b.array())
    }
}
