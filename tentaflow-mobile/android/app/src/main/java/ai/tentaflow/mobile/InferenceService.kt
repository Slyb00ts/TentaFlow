package ai.tentaflow.mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder

/**
 * Foreground service do uruchamiania inferencji w tle.
 * Zapobiega zabijaniu procesu przez system Android.
 */
class InferenceService : Service() {

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val notification = buildNotification()
        startForeground(NOTIFICATION_ID, notification)
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "TentaFlow Inference",
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "Inference model is running"
            }
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(): Notification {
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("TentaFlow")
            .setContentText("Model inference is running")
            .setSmallIcon(android.R.drawable.ic_menu_manage)
            .build()
    }

    companion object {
        private const val CHANNEL_ID = "tentaflow_inference"
        private const val NOTIFICATION_ID = 1
    }
}
