package com.zytunes.zytunes_mobile

import android.content.Intent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "start" -> {
                        val intent =
                            Intent(this, PlaybackService::class.java).apply {
                                action = PlaybackService.ACTION_START
                                putExtra(
                                    PlaybackService.EXTRA_TITLE,
                                    call.argument<String>("title") ?: "zytunes",
                                )
                                putExtra(
                                    PlaybackService.EXTRA_ARTIST,
                                    call.argument<String>("artist") ?: "",
                                )
                            }
                        startForegroundService(intent)
                        result.success(null)
                    }
                    "stop" -> {
                        startService(
                            Intent(this, PlaybackService::class.java).apply {
                                action = PlaybackService.ACTION_STOP
                            },
                        )
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
    }

    companion object {
        private const val CHANNEL = "zytunes/playback_keepalive"
    }
}
