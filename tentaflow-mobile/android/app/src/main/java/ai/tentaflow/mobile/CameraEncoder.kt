package ai.tentaflow.mobile

import android.content.Context
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface

/**
 * Kamera telefonu → enkoder H.264 (MediaCodec) → ramki Annex-B → Rust core
 * ([NativeLib.pushCameraH264]). Trafia do TEGO SAMEGO potoku co każda kamera robota:
 * kafelek MSE + skrzynka klatek dla TentaVision i AI-głębi (jeden strumień, wielu
 * odbiorców). Addon `phone` rejestruje kamerę; tu tylko kodujemy i pchamy NAL-e.
 *
 * Camera2 → input Surface enkodera → wyjście kodera (Annex-B z byte-stream format).
 * Wymaga uprawnienia CAMERA (sprawdzane przez wołającego).
 */
class CameraEncoder(private val context: Context) {
    private var codec: MediaCodec? = null
    private var cameraDevice: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var inputSurface: Surface? = null
    private val thread = HandlerThread("phone-cam").apply { start() }
    private val handler = Handler(thread.looper)
    @Volatile private var running = false

    private val width = 1280
    private val height = 720
    private val fps = 30
    private val bitrate = 4_000_000

    fun start(): Boolean {
        val mgr = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        val camId = mgr.cameraIdList.firstOrNull {
            mgr.getCameraCharacteristics(it)
                .get(android.hardware.camera2.CameraCharacteristics.LENS_FACING) ==
                android.hardware.camera2.CameraMetadata.LENS_FACING_BACK
        } ?: mgr.cameraIdList.firstOrNull() ?: return false

        val fmt = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
        }
        val enc = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        enc.configure(fmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        inputSurface = enc.createInputSurface()
        enc.setCallback(object : MediaCodec.Callback() {
            override fun onInputBufferAvailable(c: MediaCodec, i: Int) {}
            override fun onOutputBufferAvailable(c: MediaCodec, i: Int, info: MediaCodec.BufferInfo) {
                val buf = c.getOutputBuffer(i)
                if (buf != null && info.size > 0) {
                    val bytes = ByteArray(info.size)
                    buf.position(info.offset); buf.get(bytes)
                    // MediaCodec AVC output is Annex-B (start codes); push as-is.
                    NativeLib.pushCameraH264(bytes)
                }
                c.releaseOutputBuffer(i, false)
            }
            override fun onError(c: MediaCodec, e: MediaCodec.CodecException) {}
            override fun onOutputFormatChanged(c: MediaCodec, f: MediaFormat) {}
        }, handler)
        enc.start()
        codec = enc

        return try {
            @Suppress("MissingPermission")
            mgr.openCamera(camId, object : CameraDevice.StateCallback() {
                override fun onOpened(device: CameraDevice) {
                    cameraDevice = device
                    val surface = inputSurface ?: return
                    val req = device.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
                        addTarget(surface)
                    }
                    device.createCaptureSession(listOf(surface), object : CameraCaptureSession.StateCallback() {
                        override fun onConfigured(s: CameraCaptureSession) {
                            session = s
                            s.setRepeatingRequest(req.build(), null, handler)
                            running = true
                        }
                        override fun onConfigureFailed(s: CameraCaptureSession) {}
                    }, handler)
                }
                override fun onDisconnected(device: CameraDevice) { device.close() }
                override fun onError(device: CameraDevice, error: Int) { device.close() }
            }, handler)
            true
        } catch (e: Exception) {
            false
        }
    }

    fun stop() {
        running = false
        runCatching { session?.close() }; session = null
        runCatching { cameraDevice?.close() }; cameraDevice = null
        runCatching { codec?.stop(); codec?.release() }; codec = null
        runCatching { inputSurface?.release() }; inputSurface = null
    }
}
