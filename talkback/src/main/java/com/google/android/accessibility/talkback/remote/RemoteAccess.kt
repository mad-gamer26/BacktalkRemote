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

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.KeyEvent
import android.view.MotionEvent
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.google.android.accessibility.talkback.Feedback
import com.google.android.accessibility.talkback.Pipeline
import com.google.android.accessibility.talkback.R
import com.google.android.accessibility.talkback.TalkBackService
import com.google.android.accessibility.talkback.focusmanagement.interpreter.TouchExplorationInterpreter
import com.google.android.accessibility.utils.Performance
import com.google.android.accessibility.utils.output.FeedbackItem.FLAG_FORCE_FEEDBACK_ALL
import com.google.android.accessibility.utils.output.FeedbackItem.FLAG_NO_HISTORY
import com.google.android.accessibility.utils.output.SpeechController.QUEUE_MODE_QUEUE
import com.google.android.accessibility.utils.output.SpeechController.SpeakOptions
import com.google.android.libraries.accessibility.utils.log.LogUtils
import java.util.concurrent.Executors
import uniffi.backtalk_remote.AudioState
import uniffi.backtalk_remote.ControllerState
import uniffi.backtalk_remote.ControllerStatus
import uniffi.backtalk_remote.HostState
import uniffi.backtalk_remote.HostStatus
import uniffi.backtalk_remote.coreVersion
import uniffi.backtalk_remote.generateAccessKey

/**
 * Remote access in the running Backtalk: shares this phone while the setting is on (see
 * [RemoteHost]), and controls another phone on request (see [RemoteControl]). It tells the user
 * what happens in speech, keeps a notification while either is on, and gives the settings screen
 * the state in words.
 *
 * Everything here runs on the main thread.
 */
class RemoteAccess(
  private val service: TalkBackService,
  private val prefs: SharedPreferences,
  private val feedback: Pipeline.FeedbackReturner,
  private val interpreter: TouchExplorationInterpreter,
) : RemoteHost.Report, RemoteControl.Report {
  /** The settings screen, while it shows. */
  interface Listener {
    fun onRemoteStatusChanged()
  }

  private var host: RemoteHost? = null
  private var hostConfig: List<Any>? = null
  private var control: RemoteControl? = null
  private val listeners = LinkedHashSet<Listener>()
  private val notifications = service.getSystemService(NotificationManager::class.java)
  private var receiverRegistered = false
  private var channelCreated = false
  // Posting a notification waits on the system, up to hundreds of milliseconds: on a thread of its
  // own, in order, never on the main thread, which speech and touch need.
  private val notifier = Executors.newSingleThreadExecutor { Thread(it, "RemoteAccessNotifier") }
  // The text of each notification shown, so that an unchanged one is not posted again.
  private val notified = HashMap<Int, String>()
  private val handler = Handler(Looper.getMainLooper())
  private var shutDown = false

  /** The sharing state in words, for the settings screen. */
  var hostText: String = service.getString(R.string.remote_off)
    private set

  /** The controlling state in words, for the settings screen. */
  var controlText: String = service.getString(R.string.remote_off)
    private set

  private var lastHost: HostStatus? = null
  private var lastControl: ControllerStatus? = null

  private val prefsListener =
    SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
      if (key in SHARE_KEYS) {
        applySharing()
      }
    }

  private val receiver =
    object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
          ACTION_STOP_SHARING -> RemoteSettings.setSharing(prefs, false)
          ACTION_STOP_CONTROLLING -> stopControlling()
        }
      }
    }

  fun start() {
    prefs.registerOnSharedPreferenceChangeListener(prefsListener)
    // Loading the core, and JNA's first call into it, take a moment: not on the main thread.
    Thread(
        {
          if (RemoteNative.isAvailable(service)) {
            coreVersion()
          }
          handler.post { if (!shutDown) applySharing() }
        },
        "RemoteAccessStart",
      )
      .start()
  }

  fun shutdown() {
    shutDown = true
    prefs.unregisterOnSharedPreferenceChangeListener(prefsListener)
    stopHost()
    control?.stop()
    control = null
    cancelNotification(R.id.notification_remote_sharing)
    cancelNotification(R.id.notification_remote_controlling)
    notifier.shutdown()
    if (receiverRegistered) {
      service.unregisterReceiver(receiver)
      receiverRegistered = false
    }
  }

  fun addListener(listener: Listener) {
    listeners += listener
  }

  fun removeListener(listener: Listener) {
    listeners -= listener
  }

  /**
   * Why this phone cannot be shared, in words, or null if it can. Sharing needs Android 11, for the
   * touch passed through to apps and Backtalk's low-latency speech, which the controlling device
   * hears.
   */
  fun unavailableReason(): String? =
    when {
      Build.VERSION.SDK_INT < Build.VERSION_CODES.R ->
        service.getString(R.string.remote_share_needs_android_11)
      !RemoteNative.isAvailable(service) -> service.getString(R.string.remote_not_in_build)
      else -> null
    }

  /**
   * Why this phone cannot control another, in words, or null if it can. Controlling needs Android
   * 13, where Backtalk can get the raw touch to send on.
   */
  fun controlUnavailableReason(): String? =
    when {
      Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ->
        service.getString(R.string.remote_control_needs_android_13)
      !RemoteNative.isAvailable(service) -> service.getString(R.string.remote_not_in_build)
      !service.isServiceGestureDetectionActive ->
        service.getString(R.string.remote_needs_gesture_handling)
      else -> null
    }

  /** The access key of this phone, made the first time. Null if remote access can't run. */
  fun shareKey(): String? =
    if (unavailableReason() == null) RemoteSettings.shareKey(prefs) { generateAccessKey() } else null

  /** Replaces this phone's access key with a new one. Devices with the old one are cut off. */
  fun newShareKey(): String? {
    if (unavailableReason() != null) return null
    val key = generateAccessKey()
    prefs.edit().putString(RemoteSettings.PREF_SHARE_KEY, key).apply()
    return key
  }

  val isControlling: Boolean
    get() = control != null

  /** Starts controlling the phone with [accessKey], or returns why it can't. */
  fun startControlling(accessKey: String): String? {
    controlUnavailableReason()?.let {
      return it
    }
    uniffi.backtalk_remote.accessKeyProblem(accessKey)?.let {
      return it
    }
    prefs.edit().putString(RemoteSettings.PREF_CONTROL_KEY, accessKey.trim()).apply()
    control?.stop()
    lastControl = null
    control = RemoteControl(service, RemoteSettings.server(prefs), accessKey, this)
    return null
  }

  fun stopControlling() {
    val stopping = control ?: return
    control = null
    stopping.stop()
    lastControl = null
    controlText = service.getString(R.string.remote_off)
    speak(service.getString(R.string.remote_control_stopped))
    cancelNotification(R.id.notification_remote_controlling)
    changed()
  }

  /** The audio stream's details while controlling, in words, or null. */
  fun audioDetails(): String? = control?.audioDetails()

  /**
   * A touch event of this phone's screen, [width] by [height] pixels. Returns whether it went to
   * the phone being controlled, so that Backtalk here ignores it.
   */
  fun onLocalMotionEvent(event: MotionEvent, width: Int, height: Int): Boolean =
    control?.onMotionEvent(event, width, height) ?: false

  fun onKeyEvent(event: KeyEvent) {
    control?.onKeyEvent(event)
  }

  fun onConfigurationChanged() {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      host?.onConfigurationChanged()
    }
  }

  private fun applySharing() {
    val wanted =
      RemoteSettings.isSharing(prefs) &&
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.R &&
        RemoteNative.isAvailable(service)
    if (!wanted) {
      stopHost()
      hostText =
        if (RemoteSettings.isSharing(prefs)) unavailableReason() ?: "" else service.getString(R.string.remote_off)
      changed()
      return
    }
    val server = RemoteSettings.server(prefs)
    val key = RemoteSettings.shareKey(prefs) { generateAccessKey() }
    val playHere = RemoteSettings.playsHere(prefs)
    val config = listOf(server, key, playHere)
    if (host != null && config == hostConfig) {
      return
    }
    stopHost()
    hostConfig = config
    lastHost = null
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      host = RemoteHost(service, interpreter, feedback, server, key, playHere, this)
    }
  }

  private fun stopHost() {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      host?.stop()
    }
    host = null
    hostConfig = null
    lastHost = null
    cancelNotification(R.id.notification_remote_sharing)
  }

  override fun hostStatus(status: HostStatus) {
    if (host == null) return
    val previous = lastHost
    lastHost = status
    hostText = describe(status)
    when (status.state) {
      HostState.WAITING ->
        if (previous == null || previous.state == HostState.CONNECTING) {
          speak(service.getString(R.string.remote_sharing_ready))
        }
      HostState.FAILED -> speak(hostText)
      else -> {}
    }
    if (status.state == HostState.STOPPED || status.state == HostState.FAILED) {
      cancelNotification(R.id.notification_remote_sharing)
    } else {
      notify(
        R.id.notification_remote_sharing,
        R.string.notification_title_remote_sharing,
        hostText,
        ACTION_STOP_SHARING,
        R.string.notification_action_stop_sharing,
      )
    }
    changed()
  }

  override fun controllerStatus(status: ControllerStatus) {
    if (control == null) return
    val previous = lastControl
    lastControl = status
    controlText = describe(status)
    if (status.state != previous?.state) {
      when (status.state) {
        ControllerState.CONNECTED ->
          speak(service.getString(R.string.remote_control_started, status.phone ?: ""))
        ControllerState.WAITING_FOR_PHONE,
        ControllerState.RECONNECTING,
        ControllerState.FAILED -> speak(controlText)
        else -> {}
      }
    }
    if (status.state == ControllerState.STOPPED || status.state == ControllerState.FAILED) {
      cancelNotification(R.id.notification_remote_controlling)
      if (status.state == ControllerState.FAILED) {
        control?.stop()
        control = null
      }
    } else {
      notify(
        R.id.notification_remote_controlling,
        R.string.notification_title_remote_controlling,
        controlText,
        ACTION_STOP_CONTROLLING,
        R.string.pref_remote_stop_control,
      )
    }
    changed()
  }

  override fun stopRequested() {
    stopControlling()
  }

  override fun speak(text: CharSequence) {
    val options =
      SpeakOptions.create()
        .setQueueMode(QUEUE_MODE_QUEUE)
        .setFlags(FLAG_NO_HISTORY or FLAG_FORCE_FEEDBACK_ALL)
    feedback.returnFeedback(Performance.EVENT_ID_UNTRACKED, Feedback.speech(text, options))
  }

  private fun describe(status: HostStatus): String {
    val text =
      when (status.state) {
        HostState.CONNECTING -> service.getString(R.string.remote_host_connecting)
        HostState.WAITING -> service.getString(R.string.remote_host_waiting)
        HostState.CONTROLLED ->
          service.getString(R.string.remote_host_controlled, status.controllers.joinToString(", "))
        HostState.RECONNECTING -> service.getString(R.string.remote_reconnecting)
        HostState.STOPPED -> service.getString(R.string.remote_off)
        HostState.FAILED -> return service.getString(R.string.remote_failed, status.detail ?: "")
      }
    return withDetail(text, status.detail)
  }

  private fun describe(status: ControllerStatus): String {
    val text =
      when (status.state) {
        ControllerState.CONNECTING -> service.getString(R.string.remote_host_connecting)
        ControllerState.WAITING_FOR_PHONE -> service.getString(R.string.remote_control_waiting)
        ControllerState.CONNECTED -> {
          val audio =
            when (status.audio) {
              AudioState.PLAYING -> service.getString(R.string.remote_control_audio_playing)
              AudioState.FAILED -> null
              else -> service.getString(R.string.remote_control_audio_waiting)
            }
          listOfNotNull(
              service.getString(R.string.remote_control_connected, status.phone ?: ""),
              audio,
            )
            .joinToString(". ")
        }
        ControllerState.RECONNECTING -> service.getString(R.string.remote_reconnecting)
        ControllerState.STOPPED -> service.getString(R.string.remote_off)
        ControllerState.FAILED -> return service.getString(R.string.remote_failed, status.detail ?: "")
      }
    return withDetail(text, status.detail)
  }

  private fun withDetail(text: String, detail: String?): String =
    if (detail.isNullOrBlank()) text
    else service.getString(R.string.remote_status_with_detail, text, detail)

  private fun changed() {
    for (listener in listeners.toList()) {
      listener.onRemoteStatusChanged()
    }
  }

  private fun cancelNotification(id: Int) {
    notified.remove(id)
    val manager = notifications ?: return
    notifier.execute { manager.cancel(id) }
  }

  private fun notify(id: Int, title: Int, text: String, stopAction: String, stopLabel: Int) {
    val manager = notifications ?: return
    if (!receiverRegistered) {
      val filter =
        IntentFilter().apply {
          addAction(ACTION_STOP_SHARING)
          addAction(ACTION_STOP_CONTROLLING)
        }
      ContextCompat.registerReceiver(service, receiver, filter, ContextCompat.RECEIVER_NOT_EXPORTED)
      receiverRegistered = true
    }
    if (notified[id] == text) {
      return
    }
    val channel =
      if (channelCreated) {
        null
      } else {
        channelCreated = true
        NotificationChannel(
          NOTIFICATION_CHANNEL,
          service.getString(R.string.notification_channel_remote_access),
          NotificationManager.IMPORTANCE_LOW,
        )
      }
    val stop =
      PendingIntent.getBroadcast(
        service,
        id,
        Intent(stopAction).setPackage(service.packageName),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
      )
    val notification =
      NotificationCompat.Builder(service, NOTIFICATION_CHANNEL)
        .setSmallIcon(R.drawable.quantum_gm_ic_accessibility_new_vd_theme_24)
        .setContentTitle(service.getString(title))
        .setContentText(text)
        .setStyle(NotificationCompat.BigTextStyle().bigText(text))
        .addAction(0, service.getString(stopLabel), stop)
        .setOngoing(true)
        .setSilent(true)
        .setShowWhen(false)
        .setOnlyAlertOnce(true)
        .setCategory(NotificationCompat.CATEGORY_STATUS)
        .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
        .build()
    notified[id] = text
    notifier.execute {
      try {
        channel?.let { manager.createNotificationChannel(it) }
        manager.notify(id, notification)
      } catch (e: SecurityException) {
        // Without permission to post notifications, settings and the volume keys still stop it.
        LogUtils.w(TAG, "Cannot show the remote access notification: %s", e)
      }
    }
  }

  private companion object {
    const val TAG = "RemoteAccess"
    const val NOTIFICATION_CHANNEL = "remote_access"
    const val ACTION_STOP_SHARING = "com.google.android.accessibility.talkback.REMOTE_STOP_SHARING"
    const val ACTION_STOP_CONTROLLING =
      "com.google.android.accessibility.talkback.REMOTE_STOP_CONTROLLING"
    val SHARE_KEYS =
      setOf(
        RemoteSettings.PREF_SHARE,
        RemoteSettings.PREF_SERVER,
        RemoteSettings.PREF_SHARE_KEY,
        RemoteSettings.PREF_PLAY_HERE,
      )
  }
}
