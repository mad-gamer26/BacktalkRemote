/*
 * Copyright 2026 Backtalk contributors
 *
 * Licensed under the Apache License, Version 2.0 (the "License"); you may not
 * use this file except in compliance with the License. You may obtain a copy of
 * the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
 * WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
 * License for the specific language governing permissions and limitations under
 * the License.
 */

package com.google.android.accessibility.talkback.remote

import android.accessibilityservice.AccessibilityService
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.util.Size
import android.view.WindowManager
import androidx.annotation.RequiresApi
import com.google.android.accessibility.talkback.Pipeline
import com.google.android.accessibility.talkback.R
import com.google.android.accessibility.talkback.TalkBackService
import com.google.android.accessibility.talkback.focusmanagement.interpreter.TouchExplorationInterpreter
import com.google.android.libraries.accessibility.utils.log.LogUtils
import uniffi.backtalk_remote.GlobalAction
import uniffi.backtalk_remote.HostListener
import uniffi.backtalk_remote.HostSession
import uniffi.backtalk_remote.HostState
import uniffi.backtalk_remote.HostStatus
import uniffi.backtalk_remote.Screen
import uniffi.backtalk_remote.TouchEvent

/**
 * Shares this phone: devices with its access key control it through the relay, their touches go to
 * Backtalk as if they were this phone's own, and Backtalk's audio goes to them.
 *
 * While a device controls the phone, the screen stays on, so that the phone can be used from afar.
 * The session's callbacks come on its own threads; everything here runs on the main thread.
 */
@RequiresApi(Build.VERSION_CODES.TIRAMISU)
class RemoteHost(
  private val service: TalkBackService,
  interpreter: TouchExplorationInterpreter,
  feedback: Pipeline.FeedbackReturner,
  private val server: String,
  private val accessKey: String,
  private val playHere: Boolean,
  private val report: Report,
) : HostListener {
  /** What remote access tells the user. */
  interface Report {
    fun hostStatus(status: HostStatus)

    fun speak(text: CharSequence)
  }

  private val handler = Handler(Looper.getMainLooper())
  private val touch = RemoteTouchController(service, interpreter, feedback)
  private val input =
    RemoteTouchInput(::screenSize, touch::onMotionEvent, cancelled = touch::cancel)
  private val feeder = RemoteAudioFeeder(service) { rate, pcm -> session.pushAudio(rate.toUInt(), pcm) }
  private val session: HostSession =
    HostSession.start(server, accessKey, RemoteSettings.deviceName(service), screen(), this)
  private var controllers: List<String> = emptyList()
  private var wakeLock: PowerManager.WakeLock? = null
  @Volatile private var stopped = false

  fun stop() {
    stopped = true
    session.stop()
    feeder.stop()
    input.stop()
    touch.stop()
    releaseWakeLock()
  }

  /** The screen turned or changed size. */
  fun onConfigurationChanged() {
    session.setScreen(screen())
  }

  override fun onStatus(status: HostStatus) {
    handler.post {
      if (stopped && status.state != HostState.STOPPED) return@post
      announceChanges(status)
      report.hostStatus(status)
    }
  }

  override fun onTouch(controller: UInt, event: TouchEvent) {
    if (!stopped) input.onTouch(controller, event)
  }

  override fun onGlobalAction(action: GlobalAction) {
    handler.post {
      if (stopped) return@post
      service.performGlobalAction(
        when (action) {
          GlobalAction.BACK -> AccessibilityService.GLOBAL_ACTION_BACK
          GlobalAction.HOME -> AccessibilityService.GLOBAL_ACTION_HOME
          GlobalAction.RECENTS -> AccessibilityService.GLOBAL_ACTION_RECENTS
          GlobalAction.NOTIFICATIONS -> AccessibilityService.GLOBAL_ACTION_NOTIFICATIONS
          GlobalAction.QUICK_SETTINGS -> AccessibilityService.GLOBAL_ACTION_QUICK_SETTINGS
        }
      )
    }
  }

  override fun onTypeText(text: String) {
    handler.post {
      if (stopped) return@post
      when (RemoteTyping.type(service, text)) {
        RemoteTyping.Result.TYPED -> {}
        RemoteTyping.Result.NO_FIELD -> report.speak(service.getString(R.string.remote_type_no_field))
        RemoteTyping.Result.FAILED -> report.speak(service.getString(R.string.remote_type_failed))
      }
    }
  }

  override fun onAudioWanted(wanted: Boolean) {
    handler.post {
      if (wanted && !stopped) {
        feeder.start(playHere)
        acquireWakeLock()
      } else {
        feeder.stop()
        input.stop()
        releaseWakeLock()
      }
    }
  }

  /** Says who started and stopped controlling the phone. */
  private fun announceChanges(status: HostStatus) {
    val now = if (status.state == HostState.CONTROLLED) status.controllers else emptyList()
    for (name in now - controllers.toSet()) {
      report.speak(service.getString(R.string.remote_controller_joined, name))
    }
    for (name in controllers - now.toSet()) {
      report.speak(service.getString(R.string.remote_controller_left, name))
    }
    controllers = now
  }

  @Suppress("DEPRECATION") // The only way for a service to turn the screen on and keep it on.
  private fun acquireWakeLock() {
    if (wakeLock != null) return
    val power = service.getSystemService(PowerManager::class.java) ?: return
    wakeLock =
      power
        .newWakeLock(
          PowerManager.SCREEN_BRIGHT_WAKE_LOCK or PowerManager.ACQUIRE_CAUSES_WAKEUP,
          "Backtalk:RemoteControl",
        )
        .apply {
          setReferenceCounted(false)
          try {
            acquire(MAX_WAKE_MS)
          } catch (e: SecurityException) {
            LogUtils.w(TAG, "Cannot keep the screen on: %s", e)
          }
        }
  }

  private fun releaseWakeLock() {
    wakeLock?.let { if (it.isHeld) it.release() }
    wakeLock = null
  }

  private fun screenSize(): Size {
    val bounds = service.getSystemService(WindowManager::class.java).maximumWindowMetrics.bounds
    return Size(bounds.width(), bounds.height())
  }

  private fun screen(): Screen {
    val size = screenSize()
    return Screen(size.width.toUInt(), size.height.toUInt())
  }

  companion object {
    private const val TAG = "RemoteHost"
    /** A wake lock needs a limit. After this, the screen can turn off as usual. */
    private const val MAX_WAKE_MS = 4 * 60 * 60 * 1000L
  }
}
