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

import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Size
import android.view.InputDevice
import android.view.MotionEvent
import uniffi.backtalk_remote.TouchAction
import uniffi.backtalk_remote.TouchEvent

/**
 * Turns the touches of controlling devices into this phone's motion events, and hands them to
 * [deliver] on the main thread.
 *
 * Touch events cross the network unevenly, but Backtalk's gestures depend on their timing: a double
 * tap is two taps within a moment, and a swipe is a fast move. So each event plays at the time it
 * had on the controlling device, shifted by the network's delay plus [JITTER_MS] to absorb its
 * changes. An event that comes later than that plays at once, and the events after it keep their
 * spacing from it.
 *
 * One device touches at a time: while one's touch is in progress, another's is ignored. Positions
 * come from 0 to 1 across the controlling device's touch area and are spread over this phone's
 * whole screen, as it is turned now.
 */
class RemoteTouchInput(
  private val screenSize: () -> Size,
  private val deliver: (MotionEvent) -> Unit,
  private val cancelled: () -> Unit,
) {
  private val handler = Handler(Looper.getMainLooper())
  private val lock = Any()

  // Timing, per controlling device, on any thread.
  private class Clock(var offset: Long, var lastArrival: Long, var lastTarget: Long)

  private val clocks = HashMap<UInt, Clock>()

  // On the main thread: the device whose touch is in progress, and its pointer IDs, as given by the
  // device and as given to Backtalk (0 to 9).
  private var active: UInt? = null
  private val pointerIds = HashMap<UInt, Int>()
  private var downTime = 0L

  /** A touch event from [controller]. Any thread. */
  fun onTouch(controller: UInt, event: TouchEvent) {
    val now = SystemClock.uptimeMillis()
    val sent = event.timeMs.toLong()
    val target =
      synchronized(lock) {
        val clock = clocks.getOrPut(controller) { Clock(now - sent, 0, 0) }
        if (now - clock.lastArrival > IDLE_MS) {
          // Nothing for a while: start again from this event's delay.
          clock.offset = now - sent
        } else if (sent + clock.offset + JITTER_MS < now) {
          // Later than planned: the network is slower now. Carry on from here.
          clock.offset = now - JITTER_MS - sent
        }
        clock.lastArrival = now
        val target = maxOf(sent + clock.offset + JITTER_MS, clock.lastTarget, now)
        clock.lastTarget = target
        target
      }
    handler.postAtTime({ play(controller, event) }, this, target)
  }

  /** [controller] left: its touch ends, if one is in progress. */
  fun forget(controller: UInt) {
    synchronized(lock) { clocks.remove(controller) }
    handler.post {
      if (active == controller) {
        cancelTouch()
      }
    }
  }

  /** Drops everything waiting, and ends a touch in progress. Main thread. */
  fun stop() {
    handler.removeCallbacksAndMessages(this)
    synchronized(lock) { clocks.clear() }
    if (active != null) {
      cancelTouch()
    }
  }

  private fun cancelTouch() {
    active = null
    pointerIds.clear()
    cancelled()
  }

  private fun play(controller: UInt, event: TouchEvent) {
    if (event.action == TouchAction.DOWN) {
      if (active != null) {
        if (active != controller) {
          // Another device is touching.
          return
        }
        // A new touch while the last never ended: its end was lost.
        cancelTouch()
      }
      active = controller
      pointerIds.clear()
    }
    if (active != controller) {
      return
    }
    if (event.action == TouchAction.DOWN || event.action == TouchAction.POINTER_DOWN) {
      val free = (0 until MAX_POINTERS).firstOrNull { it !in pointerIds.values } ?: return
      pointerIds[event.changed] = free
    }
    val motion = toMotionEvent(event) ?: return
    if (event.action == TouchAction.DOWN) {
      downTime = motion.eventTime
    }
    try {
      deliver(motion)
    } finally {
      motion.recycle()
    }
    when (event.action) {
      TouchAction.POINTER_UP -> pointerIds.remove(event.changed)
      TouchAction.UP,
      TouchAction.CANCEL -> {
        active = null
        pointerIds.clear()
      }
      else -> {}
    }
  }

  private fun toMotionEvent(event: TouchEvent): MotionEvent? {
    val screen = screenSize()
    val pointers =
      event.pointers
        .map { (pointerIds[it.id] ?: return null) to it }
        .sortedBy { it.first }
    val properties =
      Array(pointers.size) { i ->
        MotionEvent.PointerProperties().apply {
          id = pointers[i].first
          toolType = MotionEvent.TOOL_TYPE_FINGER
        }
      }
    val coords =
      Array(pointers.size) { i ->
        MotionEvent.PointerCoords().apply {
          x = (pointers[i].second.x * screen.width).coerceIn(0f, screen.width - 1f)
          y = (pointers[i].second.y * screen.height).coerceIn(0f, screen.height - 1f)
          pressure = 1f
          size = 1f
        }
      }
    val masked =
      when (event.action) {
        TouchAction.DOWN -> MotionEvent.ACTION_DOWN
        TouchAction.POINTER_DOWN -> MotionEvent.ACTION_POINTER_DOWN
        TouchAction.MOVE -> MotionEvent.ACTION_MOVE
        TouchAction.POINTER_UP -> MotionEvent.ACTION_POINTER_UP
        TouchAction.UP -> MotionEvent.ACTION_UP
        TouchAction.CANCEL -> MotionEvent.ACTION_CANCEL
      }
    val action =
      if (masked == MotionEvent.ACTION_POINTER_DOWN || masked == MotionEvent.ACTION_POINTER_UP) {
        val changed = pointerIds[event.changed] ?: return null
        val index = pointers.indexOfFirst { it.first == changed }
        if (index < 0) return null
        masked or (index shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
      } else {
        masked
      }
    val now = SystemClock.uptimeMillis()
    return MotionEvent.obtain(
      if (masked == MotionEvent.ACTION_DOWN) now else downTime,
      now,
      action,
      pointers.size,
      properties,
      coords,
      /* metaState= */ 0,
      /* buttonState= */ 0,
      /* xPrecision= */ 1f,
      /* yPrecision= */ 1f,
      /* deviceId= */ 0,
      /* edgeFlags= */ 0,
      InputDevice.SOURCE_TOUCHSCREEN,
      /* flags= */ 0,
    )
  }

  private companion object {
    const val MAX_POINTERS = 10
    /** Added to the network's delay, to absorb its changes. */
    const val JITTER_MS = 40L
    /** After this long without an event, timing starts again from the next one. */
    const val IDLE_MS = 1_000L
  }
}
