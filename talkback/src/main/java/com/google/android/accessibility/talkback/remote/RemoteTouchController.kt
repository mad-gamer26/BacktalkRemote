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
import android.accessibilityservice.GestureDescription
import android.accessibilityservice.TouchInteractionController.STATE_CLEAR
import android.accessibilityservice.TouchInteractionController.STATE_DELEGATING
import android.accessibilityservice.TouchInteractionController.STATE_DRAGGING
import android.accessibilityservice.TouchInteractionController.STATE_TOUCH_EXPLORING
import android.accessibilityservice.TouchInteractionController.STATE_TOUCH_INTERACTING
import android.graphics.Path
import android.graphics.PointF
import android.graphics.Rect
import android.graphics.Region
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.Parcel
import android.os.SystemClock
import android.view.MotionEvent
import android.view.ViewConfiguration
import android.view.WindowManager
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import androidx.annotation.RequiresApi
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import com.google.android.accessibility.talkback.Feedback
import com.google.android.accessibility.talkback.Pipeline
import com.google.android.accessibility.talkback.TalkBackService
import com.google.android.accessibility.talkback.TouchController
import com.google.android.accessibility.talkback.TouchInteractionMonitor
import com.google.android.accessibility.talkback.focusmanagement.interpreter.TouchExplorationInterpreter
import com.google.android.accessibility.utils.Performance
import com.google.android.libraries.accessibility.utils.log.LogUtils
import java.util.concurrent.Executors
import kotlin.math.hypot
import kotlin.math.max

/**
 * Plays the part of Android's touch explorer for touches that come from a controlling device, so
 * that Backtalk handles them exactly as it handles its own touch screen: a [TouchInteractionMonitor]
 * of their own runs Backtalk's gesture detection on them, with the same timing, and asks this
 * controller, as it asks Android's, to explore by touch, to click, or to pass the touch on to the
 * app.
 *
 * It follows Android's TouchExplorer as Backtalk uses it, with gestures detected by the service:
 * * The first finger down starts an interaction: TOUCH_INTERACTION_START, then the state
 *   TOUCH_INTERACTING, then the event. When the last finger lifts, the interaction ends a double
 *   tap timeout later, unless another finger comes down first, so that multi-tap gestures stay in
 *   one interaction.
 * * Exploring sends the item under the finger to Backtalk as a hover would, rather than passing
 *   one-finger moves on, and passes a second finger's events on once it has moved past the double
 *   tap slop.
 * * Passing a touch to the app (dragging, delegating, double tap and hold) has no way to inject a
 *   touch as it happens, so the touch is recorded and played to the app with [dispatchGesture] once
 *   the fingers lift. One finger is played with its own timing, so a hold stays a hold; several are
 *   played each at an even speed over the time it was down. Android explores played touches like
 *   any other, so while one plays, the whole screen passes touch through (the lock that the braille
 *   keyboard uses), and direct touch's region comes back after.
 * * Android tells the monitor of a state it asked for after the request returns. Such changes wait
 *   for the main thread, but always reach the monitor before the next event or state change, as
 *   Android's do.
 *
 * Runs on the main thread, as the monitor does.
 */
@RequiresApi(Build.VERSION_CODES.TIRAMISU)
class RemoteTouchController(
  private val service: TalkBackService,
  private val interpreter: TouchExplorationInterpreter,
  private val feedback: Pipeline.FeedbackReturner,
) : TouchController {
  private val handler = Handler(Looper.getMainLooper())
  private val monitor: TouchInteractionMonitor = service.createRemoteTouchMonitor(this)
  private val doubleTapSlop = ViewConfiguration.get(service).scaledDoubleTapSlop
  private var state = STATE_CLEAR
  private var lastEvent: MotionEvent? = null
  private var passthrough: Passthrough? = null
  // Where each pointer went down, for the double tap slop while exploring.
  private val downX = FloatArray(MAX_POINTERS)
  private val downY = FloatArray(MAX_POINTERS)
  private val endInteraction = Runnable { endInteraction() }
  // States the monitor asked for, not yet told to it, oldest first.
  private val pendingStates = ArrayDeque<Int>()
  private val flushStates = Runnable { flushStates() }
  // Touches being played to the app, while the screen passes touch through.
  private var playing = 0
  private val unlockAfterAll = Runnable {
    playing = 0
    setPassThrough(false)
  }
  private val hitTests = Executors.newSingleThreadExecutor { Thread(it, "RemoteHitTest") }
  private val hoverLock = Any()
  private var hoverAt: PointF? = null
  private var hoverRunning = false

  override fun getMaxPointerCount(): Int = MAX_POINTERS

  override fun getState(): Int = state

  /** One event of a remote touch, in this phone's screen coordinates. */
  fun onMotionEvent(event: MotionEvent) {
    flushStates()
    val action = event.actionMasked
    if (action == MotionEvent.ACTION_DOWN) {
      handler.removeCallbacks(endInteraction)
      if (state == STATE_CLEAR) {
        sendInteractionEvent(AccessibilityEvent.TYPE_TOUCH_INTERACTION_START)
        changeState(STATE_TOUCH_INTERACTING)
      }
    }
    if (action == MotionEvent.ACTION_DOWN || action == MotionEvent.ACTION_POINTER_DOWN) {
      val index = event.actionIndex
      val id = event.getPointerId(index)
      downX[id] = event.getX(index)
      downY[id] = event.getY(index)
    }
    lastEvent?.recycle()
    lastEvent = MotionEvent.obtain(event)
    when (state) {
      STATE_TOUCH_INTERACTING -> monitor.onMotionEvent(event)
      STATE_TOUCH_EXPLORING -> explore(event)
      STATE_DRAGGING -> {
        passthrough?.add(event)
        if (action == MotionEvent.ACTION_MOVE || action == MotionEvent.ACTION_POINTER_DOWN) {
          monitor.onMotionEvent(event)
        }
      }
      STATE_DELEGATING -> passthrough?.add(event)
      else -> {}
    }
    when (action) {
      MotionEvent.ACTION_UP -> lifted()
      MotionEvent.ACTION_CANCEL -> cancel()
      else -> {}
    }
  }

  /** Ends a remote touch that will not finish, such as when its device leaves. */
  fun cancel() {
    handler.removeCallbacks(endInteraction)
    passthrough = null
    if (state != STATE_CLEAR) {
      endInteraction()
    }
  }

  fun stop() {
    cancel()
    if (playing > 0) {
      unlockAfterAll.run()
    }
    handler.removeCallbacksAndMessages(null)
    monitor.stop()
    service.releaseRemoteTouchMonitor(monitor)
    hitTests.shutdownNow()
    lastEvent?.recycle()
    lastEvent = null
  }

  private fun lifted() {
    when (state) {
      STATE_DRAGGING,
      STATE_DELEGATING -> {
        passthrough?.play()
        passthrough = null
        endInteraction()
      }
      else -> {
        // Another finger may come down for the next tap of a multi-tap gesture.
        handler.removeCallbacks(endInteraction)
        handler.postDelayed(endInteraction, ViewConfiguration.getDoubleTapTimeout().toLong())
      }
    }
  }

  private fun endInteraction() {
    handler.removeCallbacks(endInteraction)
    sendInteractionEvent(AccessibilityEvent.TYPE_TOUCH_INTERACTION_END)
    changeState(STATE_CLEAR)
  }

  private fun explore(event: MotionEvent) {
    when (event.actionMasked) {
      // A finger down again soon after lifting carries on exploring.
      MotionEvent.ACTION_DOWN -> hover(event.getX(0), event.getY(0))
      MotionEvent.ACTION_POINTER_DOWN -> monitor.onMotionEvent(event)
      MotionEvent.ACTION_MOVE ->
        when (event.pointerCount) {
          1 -> hover(event.getX(0), event.getY(0))
          2 -> {
            // A second finger that stays near may be the second tap of a double tap.
            val id = event.getPointerId(0)
            if (hypot(downX[id] - event.getX(0), downY[id] - event.getY(0)) > doubleTapSlop) {
              monitor.onMotionEvent(event)
            }
          }
          else -> monitor.onMotionEvent(event)
        }
      else -> {}
    }
  }

  override fun requestTouchExploration() {
    if (state != STATE_TOUCH_INTERACTING) {
      return
    }
    state = STATE_TOUCH_EXPLORING
    // Android tells the monitor after the request returns, never during it.
    postState(STATE_TOUCH_EXPLORING)
    // Where the finger is, or was as it lifted: a tap that turned out not to be a gesture explores
    // what was under it.
    val event = lastEvent ?: return
    hover(event.getX(0), event.getY(0))
  }

  override fun requestDragging(pointerId: Int) {
    val event = lastEvent
    if (state != STATE_TOUCH_INTERACTING || event == null || event.findPointerIndex(pointerId) < 0) {
      return
    }
    state = STATE_DRAGGING
    passthrough = Passthrough(event, onlyPointer = pointerId, offset = null)
    postState(STATE_DRAGGING)
  }

  override fun requestDelegating() {
    val event = lastEvent
    if ((state != STATE_TOUCH_INTERACTING && state != STATE_DRAGGING) || event == null) {
      return
    }
    state = STATE_DELEGATING
    passthrough = Passthrough(event, onlyPointer = null, offset = null)
    postState(STATE_DELEGATING)
  }

  override fun performClick() {
    val focus = service.findFocus(AccessibilityNodeInfo.FOCUS_ACCESSIBILITY) ?: return
    if (!focus.performAction(AccessibilityNodeInfo.ACTION_CLICK)) {
      // As Android does when the item can't be clicked: a tap where it is.
      val point = centerOf(focus) ?: return
      dispatch(tapAt(point, ViewConfiguration.getTapTimeout().toLong()))
    }
  }

  override fun performLongClickAndStartDrag() {
    val focus = service.findFocus(AccessibilityNodeInfo.FOCUS_ACCESSIBILITY) ?: return
    val point = centerOf(focus) ?: return
    val event = lastEvent ?: return
    // As Android does: a touch held on the item, which then follows the finger. It plays when the
    // finger lifts, held at least long enough to be a long press.
    state = STATE_DELEGATING
    passthrough =
      Passthrough(
        event,
        onlyPointer = event.getPointerId(0),
        offset = PointF(point.x - event.getX(0), point.y - event.getY(0)),
        minimumHoldMs = ViewConfiguration.getLongPressTimeout() + LONG_PRESS_MARGIN_MS,
      )
    postState(STATE_DELEGATING)
  }

  private fun changeState(newState: Int) {
    flushStates()
    state = newState
    notifyState(newState)
  }

  /** Tells the monitor of [newState], which it asked for, once its request has returned. */
  private fun postState(newState: Int) {
    pendingStates.addLast(newState)
    handler.post(flushStates)
  }

  private fun flushStates() {
    handler.removeCallbacks(flushStates)
    while (pendingStates.isNotEmpty()) {
      notifyState(pendingStates.removeFirst())
    }
  }

  private fun notifyState(newState: Int) {
    monitor.onStateChanged(newState)
  }

  /**
   * Gives Backtalk the start or end of a touch interaction, as Android does for the touch screen,
   * so that everything that listens for them (speech, hints, the volume keys, the reading order
   * cache) hears remote touches too.
   *
   * Events from Android are sealed, and Backtalk's processors read them in ways only a sealed event
   * allows, while only Android can seal one. An event in a parcel carries its sealed flag twice: the
   * event's first, and its record's last, just before the count of its other records, which is 0
   * here. With both set, the event read back is sealed as Android's are, and having no source, its
   * source is null. If that ever fails, only touch exploration hears the interaction.
   */
  private fun sendInteractionEvent(type: Int) {
    val sealed = sealedEvent(type)
    if (sealed != null) {
      service.onAccessibilityEvent(sealed)
    } else if (type == AccessibilityEvent.TYPE_TOUCH_INTERACTION_START) {
      interpreter.onRemoteTouchStart()
    } else {
      interpreter.onRemoteTouchEnd()
    }
  }

  private fun sealedEvent(type: Int): AccessibilityEvent? {
    val event = AccessibilityEvent(type)
    event.eventTime = SystemClock.uptimeMillis()
    event.packageName = service.packageName
    val parcel = Parcel.obtain()
    try {
      event.writeToParcel(parcel, 0)
      val size = parcel.dataSize()
      parcel.setDataPosition(0)
      val eventSealed = parcel.readInt()
      val eventType = parcel.readInt()
      parcel.setDataPosition(size - 2 * Int.SIZE_BYTES)
      val recordSealed = parcel.readInt()
      val recordCount = parcel.readInt()
      if (eventSealed != 0 || eventType != type || recordSealed != 0 || recordCount != 0) {
        LogUtils.e(TAG, "Unexpected accessibility event layout")
        return null
      }
      parcel.setDataPosition(0)
      parcel.writeInt(1)
      parcel.setDataPosition(size - 2 * Int.SIZE_BYTES)
      parcel.writeInt(1)
      parcel.setDataPosition(0)
      val sealed = AccessibilityEvent.CREATOR.createFromParcel(parcel)
      // Throws if it is not sealed after all.
      sealed.source
      return sealed
    } catch (e: RuntimeException) {
      LogUtils.e(TAG, "Cannot make a sealed touch interaction event: %s", e)
      return null
    } finally {
      parcel.recycle()
    }
  }

  /**
   * Sends Backtalk the item at ([x], [y]), as a hover from the touch screen. Finding it asks the
   * apps, so it runs on a thread of its own; while it does, only the newest position waits.
   */
  private fun hover(x: Float, y: Float) {
    synchronized(hoverLock) {
      hoverAt = PointF(x, y)
      if (hoverRunning) {
        return
      }
      hoverRunning = true
    }
    hitTests.execute {
      while (true) {
        val point =
          synchronized(hoverLock) {
            val p = hoverAt
            hoverAt = null
            if (p == null) {
              hoverRunning = false
            }
            p
          } ?: return@execute
        val node =
          try {
            nodeAt(point.x.toInt(), point.y.toInt())
          } catch (e: RuntimeException) {
            // A window that went away while it was searched.
            LogUtils.w(TAG, "Hit test failed: %s", e)
            null
          }
        handler.post {
          if (state == STATE_TOUCH_EXPLORING) {
            monitor.onHoverEntered()
            interpreter.onRemoteHover(node, null)
          }
        }
      }
    }
  }

  /** The deepest visible node at ([x], [y]) in the top-most window there, or null. */
  private fun nodeAt(x: Int, y: Int): AccessibilityNodeInfoCompat? {
    val bounds = Rect()
    for (window in service.windows.sortedByDescending { it.layer }) {
      window.getBoundsInScreen(bounds)
      if (!bounds.contains(x, y)) {
        continue
      }
      val root = window.root ?: continue
      deepestAt(root, x, y, bounds, depth = 0)?.let {
        return AccessibilityNodeInfoCompat.wrap(it)
      }
    }
    return null
  }

  private fun deepestAt(
    node: AccessibilityNodeInfo,
    x: Int,
    y: Int,
    bounds: Rect,
    depth: Int,
  ): AccessibilityNodeInfo? {
    node.getBoundsInScreen(bounds)
    if (!node.isVisibleToUser || !bounds.contains(x, y)) {
      return null
    }
    if (depth < MAX_DEPTH) {
      // Later children are drawn over earlier ones.
      for (i in node.childCount - 1 downTo 0) {
        val child = node.getChild(i) ?: continue
        deepestAt(child, x, y, bounds, depth + 1)?.let {
          return it
        }
      }
    }
    return node
  }

  private fun centerOf(node: AccessibilityNodeInfo): PointF? {
    val bounds = Rect()
    node.getBoundsInScreen(bounds)
    return if (bounds.isEmpty) null else PointF(bounds.exactCenterX(), bounds.exactCenterY())
  }

  private fun tapAt(point: PointF, durationMs: Long): GestureDescription =
    GestureDescription.Builder()
      .addStroke(
        GestureDescription.StrokeDescription(
          Path().apply { moveTo(point.x, point.y) },
          0,
          max(1L, durationMs),
        )
      )
      .build()

  /**
   * Plays [gesture] to the app, the whole screen passing touch through until it ends. [then] runs
   * when it ends, with whether it completed.
   */
  private fun dispatch(gesture: GestureDescription, then: ((Boolean) -> Unit)? = null) {
    if (playing++ == 0) {
      setPassThrough(true)
    }
    // In case Android never reports the end.
    handler.removeCallbacks(unlockAfterAll)
    handler.postDelayed(unlockAfterAll, gesture.duration() + UNLOCK_MARGIN_MS)
    val callback = PlayedCallback(then)
    if (!service.dispatchGesture(gesture, callback, handler)) {
      LogUtils.w(TAG, "The touch could not be passed to the app")
      callback.ended(false)
    }
  }

  private inner class PlayedCallback(private val then: ((Boolean) -> Unit)?) :
    AccessibilityService.GestureResultCallback() {
    override fun onCompleted(gestureDescription: GestureDescription) = ended(true)

    override fun onCancelled(gestureDescription: GestureDescription) = ended(false)

    fun ended(completed: Boolean) {
      // The next part of a played touch locks before this one unlocks.
      then?.invoke(completed)
      if (playing > 0 && --playing == 0) {
        handler.removeCallbacks(unlockAfterAll)
        setPassThrough(false)
      }
    }
  }

  private fun setPassThrough(on: Boolean) {
    val region =
      if (on) {
        Region(service.getSystemService(WindowManager::class.java).maximumWindowMetrics.bounds)
      } else {
        Region()
      }
    feedback.returnFeedback(
      Performance.EVENT_ID_UNTRACKED,
      Feedback.passThroughMode(Feedback.PassThroughMode.Action.LOCK_PASS_THROUGH, region),
    )
  }

  private fun GestureDescription.duration(): Long =
    (0 until strokeCount).maxOfOrNull { getStroke(it).let { s -> s.startTime + s.duration } } ?: 0L

  /** Where one pointer was, and when. */
  private class Sample(val time: Long, val x: Float, val y: Float)

  /**
   * A touch passed to the app, recorded from when it was passed on: each pointer's path, or only
   * [onlyPointer]'s, moved by [offset] when the touch acts somewhere else than the finger.
   */
  private inner class Passthrough(
    start: MotionEvent,
    private val onlyPointer: Int?,
    private val offset: PointF?,
    private val minimumHoldMs: Long = 0,
  ) {
    private val startTime = start.eventTime
    private val paths = HashMap<Int, ArrayList<Sample>>()
    private val ended = HashMap<Int, Long>()

    init {
      addAll(start)
    }

    fun add(event: MotionEvent) {
      when (event.actionMasked) {
        MotionEvent.ACTION_POINTER_UP,
        MotionEvent.ACTION_UP -> {
          addAll(event)
          ended[event.getPointerId(event.actionIndex)] = event.eventTime
        }
        MotionEvent.ACTION_CANCEL -> {}
        else -> addAll(event)
      }
    }

    private fun addAll(event: MotionEvent) {
      for (i in 0 until event.pointerCount) {
        val id = event.getPointerId(i)
        if (onlyPointer != null && id != onlyPointer) {
          continue
        }
        if (id in ended) {
          continue
        }
        val x = event.getX(i) + (offset?.x ?: 0f)
        val y = event.getY(i) + (offset?.y ?: 0f)
        paths.getOrPut(id) { ArrayList() }.add(Sample(event.eventTime, x, y))
      }
    }

    fun play() {
      val end = lastEvent?.eventTime ?: SystemClock.uptimeMillis()
      if (paths.size == 1) {
        playTimed(paths.values.first(), end)
      } else {
        playEven(end)
      }
    }

    /** Several pointers, each along its path at an even speed while it was down. */
    private fun playEven(end: Long) {
      val builder = GestureDescription.Builder()
      var strokes = 0
      for ((id, samples) in paths) {
        if (strokes == GestureDescription.getMaxStrokeCount()) {
          break
        }
        val first = samples.first()
        val path = Path().apply { moveTo(first.x, first.y) }
        for (s in samples.drop(1)) {
          path.lineTo(s.x, s.y)
        }
        val startMs = first.time - startTime
        val durationMs = max(1L, (ended[id] ?: end) - first.time)
        builder.addStroke(GestureDescription.StrokeDescription(path, startMs, durationMs))
        strokes++
      }
      if (strokes > 0) {
        dispatch(builder.build())
      }
    }

    /**
     * One pointer, in steps of about [SEGMENT_MS] that each continue the last, so that holds,
     * pauses and speed changes play as they were made.
     */
    private fun playTimed(samples: List<Sample>, end: Long) {
      val last = samples.last()
      val timed = ArrayList(samples)
      val heldUntil = max(last.time, samples.first().time + minimumHoldMs)
      if (heldUntil > last.time) {
        timed.add(Sample(heldUntil, last.x, last.y))
      } else if (end > last.time) {
        timed.add(Sample(end, last.x, last.y))
      }
      val segments = ArrayList<Pair<Path, Long>>()
      var from = timed.first()
      var path = Path().apply { moveTo(from.x, from.y) }
      for (s in timed.drop(1)) {
        path.lineTo(s.x, s.y)
        if (s.time - from.time >= SEGMENT_MS) {
          segments += path to (s.time - from.time)
          from = s
          path = Path().apply { moveTo(s.x, s.y) }
        }
      }
      if (segments.isEmpty() || timed.last() !== from) {
        segments += path to max(1L, timed.last().time - from.time)
      }
      playSegment(segments, 0, null)
    }

    private fun playSegment(
      segments: List<Pair<Path, Long>>,
      index: Int,
      previous: GestureDescription.StrokeDescription?,
    ) {
      val (path, durationMs) = segments[index]
      val more = index + 1 < segments.size
      val stroke =
        previous?.continueStroke(path, 0, max(1L, durationMs), more)
          ?: GestureDescription.StrokeDescription(path, 0, max(1L, durationMs), more)
      dispatch(GestureDescription.Builder().addStroke(stroke).build()) { completed ->
        if (more && completed) {
          playSegment(segments, index + 1, stroke)
        }
      }
    }
  }

  private companion object {
    const val TAG = "RemoteTouchController"
    /** Pointer IDs from the controlling device are made 0 to 9 on the way in. */
    const val MAX_POINTERS = 10
    const val MAX_DEPTH = 64
    /** Steps of a passed-on one-finger touch. */
    const val SEGMENT_MS = 50L
    const val LONG_PRESS_MARGIN_MS = 100L
    /** How long past a played touch's end the screen stays passing touch, if Android is silent. */
    const val UNLOCK_MARGIN_MS = 2_000L
  }
}
