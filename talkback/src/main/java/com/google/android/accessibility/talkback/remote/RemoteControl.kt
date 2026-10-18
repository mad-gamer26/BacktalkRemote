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

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.KeyEvent
import android.view.MotionEvent
import uniffi.backtalk_remote.ControllerListener
import uniffi.backtalk_remote.ControllerSession
import uniffi.backtalk_remote.ControllerState
import uniffi.backtalk_remote.ControllerStatus
import uniffi.backtalk_remote.TouchAction
import uniffi.backtalk_remote.TouchEvent
import uniffi.backtalk_remote.TouchPointer

/**
 * Controls another phone from this one. While connected, every touch on this screen goes to the
 * other phone, where its Backtalk handles it as its own, and its Backtalk is heard here: this
 * phone's touch screen becomes the other phone's.
 *
 * Touches only go once connected, starting with the next finger down, so this phone can be used
 * while waiting. Pressing volume down three times quickly, or the notification's stop action, stops
 * controlling.
 */
class RemoteControl(
  context: Context,
  server: String,
  accessKey: String,
  private val report: Report,
) : ControllerListener {
  /** What remote access tells the user. */
  interface Report {
    fun controllerStatus(status: ControllerStatus)

    /** Stop was asked for from this phone (the volume key). */
    fun stopRequested()
  }

  private val handler = Handler(Looper.getMainLooper())
  private val session = ControllerSession.start(server, accessKey, RemoteHost.deviceName(context), this)
  @Volatile private var connected = false
  // Main thread: whether the touch in progress goes to the other phone.
  private var forwarding = false
  private val stopPresses = RepeatedPress(STOP_PRESSES, STOP_WITHIN_MS)

  fun stop() {
    connected = false
    session.stop()
  }

  /** The audio stream's state in words, or null. */
  fun audioDetails(): String? = session.audioDetails()

  fun sendGlobalAction(action: uniffi.backtalk_remote.GlobalAction) = session.sendGlobalAction(action)

  override fun onStatus(status: ControllerStatus) {
    connected = status.state == ControllerState.CONNECTED
    handler.post { report.controllerStatus(status) }
  }

  /**
   * Sends a touch event of this screen, [width] by [height] pixels, to the other phone, and returns
   * whether it did, in which case Backtalk here ignores it.
   */
  fun onMotionEvent(event: MotionEvent, width: Int, height: Int): Boolean {
    val action = event.actionMasked
    if (action == MotionEvent.ACTION_DOWN) {
      forwarding = connected
    }
    if (!forwarding) {
      return false
    }
    val touchAction =
      when (action) {
        MotionEvent.ACTION_DOWN -> TouchAction.DOWN
        MotionEvent.ACTION_POINTER_DOWN -> TouchAction.POINTER_DOWN
        MotionEvent.ACTION_MOVE -> TouchAction.MOVE
        MotionEvent.ACTION_POINTER_UP -> TouchAction.POINTER_UP
        MotionEvent.ACTION_UP -> TouchAction.UP
        MotionEvent.ACTION_CANCEL -> TouchAction.CANCEL
        else -> return true
      }
    val pointers =
      (0 until event.pointerCount).map {
        TouchPointer(
          event.getPointerId(it).toUInt(),
          event.getX(it) / width.coerceAtLeast(1),
          event.getY(it) / height.coerceAtLeast(1),
        )
      }
    session.sendTouch(
      TouchEvent(
        touchAction,
        event.getPointerId(event.actionIndex).toUInt(),
        event.eventTime.toULong(),
        pointers,
      )
    )
    if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
      forwarding = false
    }
    return true
  }

  /** Watches for volume down pressed three times quickly. The key still changes the volume. */
  fun onKeyEvent(event: KeyEvent) {
    if (event.keyCode != KeyEvent.KEYCODE_VOLUME_DOWN ||
      event.action != KeyEvent.ACTION_DOWN ||
      event.repeatCount != 0
    ) {
      return
    }
    if (stopPresses.press(SystemClock.uptimeMillis())) {
      report.stopRequested()
    }
  }

  /** Counts presses of a key, and says when it was pressed [times] times within [withinMs]. */
  class RepeatedPress(private val times: Int, private val withinMs: Long) {
    private val pressed = LongArray(times)
    private var count = 0

    /** A press at [now]; true if it is the last of [times] within [withinMs], which starts over. */
    fun press(now: Long): Boolean {
      pressed[count % times] = now
      count++
      if (count >= times && now - pressed[count % times] <= withinMs) {
        count = 0
        return true
      }
      return false
    }
  }

  private companion object {
    const val STOP_PRESSES = 3
    const val STOP_WITHIN_MS = 1_500L
  }
}
