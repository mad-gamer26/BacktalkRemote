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
import android.os.Bundle
import android.os.SystemClock
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityNodeInfo.AccessibilityAction
import androidx.annotation.RequiresApi
import com.google.android.accessibility.talkback.Feedback
import com.google.android.accessibility.talkback.Pipeline
import com.google.android.accessibility.talkback.TalkBackService
import com.google.android.accessibility.utils.Performance.EVENT_ID_UNTRACKED
import com.google.android.accessibility.utils.monitor.InputModeTracker.INPUT_MODE_KEYBOARD
import com.google.android.accessibility.utils.traversal.TraversalStrategy
import uniffi.backtalk_remote.KeyAction
import uniffi.backtalk_remote.KeyInput

/**
 * Keys from a controlling device's keyboard, handled as the keys of a keyboard plugged into this
 * phone.
 *
 * Each key goes first to Backtalk, as a physical keyboard's key does, so all of Backtalk's keyboard
 * shortcuts work. A key Backtalk does not take is for the app. Android lets no service inject keys
 * into apps, so:
 * * From Android 13, while a text field has input focus, the key goes to it as a real key event,
 *   through Backtalk's accessibility input method: typing, deleting, moving the cursor, selecting,
 *   and the app's own shortcuts.
 * * Elsewhere (and in text fields before Android 13), the key does what it does on Android: the
 *   arrows and Tab move focus, Enter and Space click, Escape goes back, the system's Meta
 *   shortcuts work, and in a text field, characters are typed and the editing keys edit.
 *
 * Runs on the main thread.
 */
@RequiresApi(Build.VERSION_CODES.R)
class RemoteKeyboard(
  private val service: TalkBackService,
  private val feedback: Pipeline.FeedbackReturner,
) {
  /** Where a key that is down went, so that its repeats and its up go there too. */
  private enum class Route {
    BACKTALK,
    EDITOR,
    ACTION,
  }

  private class Held(val downTime: Long, val route: Route, val metaState: Int)

  private val held = HashMap<Int, Held>()

  fun onKey(key: KeyInput) {
    val code = key.keyCode.toInt()
    if (code <= KeyEvent.KEYCODE_UNKNOWN || code > KeyEvent.getMaxKeyCode()) return
    val now = SystemClock.uptimeMillis()
    val meta = KeyEvent.normalizeMetaState(key.metaState.toInt())
    if (key.action == KeyAction.UP) {
      val down = held.remove(code) ?: return
      release(code, down, now, meta)
      return
    }
    val previous = held[code]
    val repeat = if (previous == null) 0 else key.repeat.toInt().coerceAtLeast(1)
    val event = keyEvent(previous?.downTime ?: now, now, KeyEvent.ACTION_DOWN, code, repeat, meta)
    if (previous != null) {
      // A held key repeats the way its first press went.
      when (previous.route) {
        Route.BACKTALK -> service.onRemoteKeyEvent(event)
        Route.EDITOR -> if (!service.onRemoteKeyEvent(event)) sendToEditor(event)
        Route.ACTION -> if (!service.onRemoteKeyEvent(event)) act(event)
      }
      return
    }
    val route =
      when {
        service.onRemoteKeyEvent(event) -> Route.BACKTALK
        sendToEditor(event) -> Route.EDITOR
        else -> {
          act(event)
          Route.ACTION
        }
      }
    held[code] = Held(now, route, meta)
  }

  /** Lets go of every key still down, as when the controlling device leaves. */
  fun releaseAll() {
    val now = SystemClock.uptimeMillis()
    for ((code, down) in held.toList()) release(code, down, now, down.metaState)
    held.clear()
  }

  private fun release(code: Int, down: Held, now: Long, meta: Int) {
    val event = keyEvent(down.downTime, now, KeyEvent.ACTION_UP, code, 0, meta)
    // Backtalk sees every key's up, to keep track of what is held.
    val taken = service.onRemoteKeyEvent(event)
    if (!taken && down.route == Route.EDITOR) sendToEditor(event)
  }

  private fun keyEvent(downTime: Long, time: Long, action: Int, code: Int, repeat: Int, meta: Int) =
    KeyEvent(
      downTime,
      time,
      action,
      code,
      repeat,
      meta,
      // The virtual keyboard's key map, which every phone has, turns keys into characters.
      KeyCharacterMap.VIRTUAL_KEYBOARD,
      /* scancode= */ 0,
      // Not FLAG_KEEP_TOUCH_MODE: as a real keyboard, a key takes the app out of touch mode, so
      // that the arrows move its focus.
      /* flags= */ 0,
      InputDevice.SOURCE_KEYBOARD,
    )

  // The text field with input focus, if any.
  private fun editor(): AccessibilityNodeInfo? =
    service.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)?.takeIf { it.isEditable }

  /** Sends the key to the focused text field as a real key event, from Android 13. */
  private fun sendToEditor(event: KeyEvent): Boolean {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return false
    val connection = service.inputMethod?.currentInputConnection ?: return false
    if (editor() == null) return false
    connection.sendKeyEvent(event)
    return true
  }

  // What a key does where it cannot be sent as a key.

  private fun act(event: KeyEvent) {
    if (KeyEvent.isModifierKey(event.keyCode)) return
    if (systemShortcut(event)) return
    val editor = editor()
    if (editor != null && edit(editor, event)) return
    navigate(event)
  }

  /** Android's own keyboard shortcuts, which the system handles before any app. */
  private fun systemShortcut(event: KeyEvent): Boolean {
    val code = event.keyCode
    val action =
      when {
        event.isMetaPressed && event.isCtrlPressed && code == KeyEvent.KEYCODE_S ->
          AccessibilityService.GLOBAL_ACTION_TAKE_SCREENSHOT
        event.isMetaPressed && (code == KeyEvent.KEYCODE_ENTER || code == KeyEvent.KEYCODE_H) ->
          AccessibilityService.GLOBAL_ACTION_HOME
        event.isMetaPressed &&
          (code == KeyEvent.KEYCODE_DEL ||
            code == KeyEvent.KEYCODE_ESCAPE ||
            code == KeyEvent.KEYCODE_DPAD_LEFT) -> AccessibilityService.GLOBAL_ACTION_BACK
        (event.isMetaPressed || event.isAltPressed) && code == KeyEvent.KEYCODE_TAB ->
          AccessibilityService.GLOBAL_ACTION_RECENTS
        event.isMetaPressed && code == KeyEvent.KEYCODE_N ->
          AccessibilityService.GLOBAL_ACTION_NOTIFICATIONS
        event.isMetaPressed && code == KeyEvent.KEYCODE_S ->
          AccessibilityService.GLOBAL_ACTION_QUICK_SETTINGS
        event.isMetaPressed && code == KeyEvent.KEYCODE_L ->
          AccessibilityService.GLOBAL_ACTION_LOCK_SCREEN
        event.isMetaPressed && code == KeyEvent.KEYCODE_A ->
          AccessibilityService.GLOBAL_ACTION_ACCESSIBILITY_ALL_APPS
        code == KeyEvent.KEYCODE_HOME -> AccessibilityService.GLOBAL_ACTION_HOME
        code == KeyEvent.KEYCODE_BACK -> AccessibilityService.GLOBAL_ACTION_BACK
        code == KeyEvent.KEYCODE_APP_SWITCH -> AccessibilityService.GLOBAL_ACTION_RECENTS
        code == KeyEvent.KEYCODE_NOTIFICATION -> AccessibilityService.GLOBAL_ACTION_NOTIFICATIONS
        else -> return false
      }
    if (action == AccessibilityService.GLOBAL_ACTION_ACCESSIBILITY_ALL_APPS &&
      Build.VERSION.SDK_INT < Build.VERSION_CODES.S
    ) {
      return false
    }
    service.performGlobalAction(action)
    return true
  }

  /** Moves focus and clicks, as keys do in an app; Backtalk's focus follows, as it follows input focus. */
  private fun navigate(event: KeyEvent): Boolean {
    if (event.isCtrlPressed || event.isAltPressed || event.isMetaPressed) return false
    return when (event.keyCode) {
      KeyEvent.KEYCODE_DPAD_UP -> move(TraversalStrategy.SEARCH_FOCUS_UP)
      KeyEvent.KEYCODE_DPAD_DOWN -> move(TraversalStrategy.SEARCH_FOCUS_DOWN)
      KeyEvent.KEYCODE_DPAD_LEFT -> move(TraversalStrategy.SEARCH_FOCUS_LEFT)
      KeyEvent.KEYCODE_DPAD_RIGHT -> move(TraversalStrategy.SEARCH_FOCUS_RIGHT)
      KeyEvent.KEYCODE_TAB ->
        move(
          if (event.isShiftPressed) TraversalStrategy.SEARCH_FOCUS_BACKWARD
          else TraversalStrategy.SEARCH_FOCUS_FORWARD
        )
      KeyEvent.KEYCODE_MOVE_HOME ->
        feedback.returnFeedback(EVENT_ID_UNTRACKED, Feedback.focusTop(INPUT_MODE_KEYBOARD))
      KeyEvent.KEYCODE_MOVE_END ->
        feedback.returnFeedback(EVENT_ID_UNTRACKED, Feedback.focusBottom(INPUT_MODE_KEYBOARD))
      KeyEvent.KEYCODE_ENTER,
      KeyEvent.KEYCODE_NUMPAD_ENTER,
      KeyEvent.KEYCODE_DPAD_CENTER,
      KeyEvent.KEYCODE_SPACE ->
        feedback.returnFeedback(
          EVENT_ID_UNTRACKED,
          Feedback.focus(Feedback.Focus.Action.CLICK_CURRENT),
        )
      KeyEvent.KEYCODE_ESCAPE -> service.performGlobalAction(AccessibilityService.GLOBAL_ACTION_BACK)
      KeyEvent.KEYCODE_PAGE_UP -> scroll(forward = false)
      KeyEvent.KEYCODE_PAGE_DOWN -> scroll(forward = true)
      else -> false
    }
  }

  private fun move(direction: Int): Boolean =
    feedback.returnFeedback(
      EVENT_ID_UNTRACKED,
      Feedback.focusDirection(direction)
        .setInputMode(INPUT_MODE_KEYBOARD)
        .setDefaultToInputFocus(true)
        .setScroll(true)
        .setWrap(true),
    )

  /** Scrolls the list or page that has focus, as Page Up and Page Down do. */
  private fun scroll(forward: Boolean): Boolean {
    var node =
      service.findFocus(AccessibilityNodeInfo.FOCUS_ACCESSIBILITY)
        ?: service.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
    val action =
      if (forward) AccessibilityAction.ACTION_SCROLL_FORWARD else AccessibilityAction.ACTION_SCROLL_BACKWARD
    while (node != null) {
      if (node.actionList.contains(action)) return node.performAction(action.id)
      node = node.parent
    }
    return false
  }

  /**
   * Edits a text field without a key event: types characters, and does what the editing keys do.
   * Returns false for keys that are not for editing.
   */
  private fun edit(field: AccessibilityNodeInfo, event: KeyEvent): Boolean {
    val code = event.keyCode
    if (event.isMetaPressed || event.isAltPressed) return false
    if (event.isCtrlPressed) {
      return when (code) {
        KeyEvent.KEYCODE_A -> select(field, 0, text(field).length)
        KeyEvent.KEYCODE_C -> field.performAction(AccessibilityNodeInfo.ACTION_COPY)
        KeyEvent.KEYCODE_X -> field.performAction(AccessibilityNodeInfo.ACTION_CUT)
        KeyEvent.KEYCODE_V -> field.performAction(AccessibilityNodeInfo.ACTION_PASTE)
        KeyEvent.KEYCODE_DPAD_LEFT -> moveCursor(field, AccessibilityNodeInfo.MOVEMENT_GRANULARITY_WORD, false, event)
        KeyEvent.KEYCODE_DPAD_RIGHT -> moveCursor(field, AccessibilityNodeInfo.MOVEMENT_GRANULARITY_WORD, true, event)
        KeyEvent.KEYCODE_MOVE_HOME -> select(field, 0, 0)
        KeyEvent.KEYCODE_MOVE_END -> text(field).length.let { select(field, it, it) }
        else -> false
      }
    }
    return when (code) {
      KeyEvent.KEYCODE_DEL -> delete(field, forward = false)
      KeyEvent.KEYCODE_FORWARD_DEL -> delete(field, forward = true)
      KeyEvent.KEYCODE_DPAD_LEFT ->
        moveCursor(field, AccessibilityNodeInfo.MOVEMENT_GRANULARITY_CHARACTER, false, event)
      KeyEvent.KEYCODE_DPAD_RIGHT ->
        moveCursor(field, AccessibilityNodeInfo.MOVEMENT_GRANULARITY_CHARACTER, true, event)
      KeyEvent.KEYCODE_DPAD_UP -> moveCursor(field, AccessibilityNodeInfo.MOVEMENT_GRANULARITY_LINE, false, event)
      KeyEvent.KEYCODE_DPAD_DOWN -> moveCursor(field, AccessibilityNodeInfo.MOVEMENT_GRANULARITY_LINE, true, event)
      KeyEvent.KEYCODE_MOVE_HOME -> select(field, 0, 0)
      KeyEvent.KEYCODE_MOVE_END -> text(field).length.let { select(field, it, it) }
      KeyEvent.KEYCODE_ENTER,
      KeyEvent.KEYCODE_NUMPAD_ENTER ->
        if (field.isMultiLine) {
          type("\n")
        } else {
          field.performAction(AccessibilityAction.ACTION_IME_ENTER.id)
        }
      KeyEvent.KEYCODE_TAB,
      KeyEvent.KEYCODE_ESCAPE -> false
      else -> {
        val character = event.getUnicodeChar(event.metaState)
        if (character == 0 || character and KeyCharacterMap.COMBINING_ACCENT != 0) {
          false
        } else {
          type(String(Character.toChars(character)))
        }
      }
    }
  }

  private fun type(text: String) = RemoteTyping.type(service, text) == RemoteTyping.Result.TYPED

  private fun text(field: AccessibilityNodeInfo): String =
    if (field.isShowingHintText) "" else field.text?.toString() ?: ""

  private fun select(field: AccessibilityNodeInfo, start: Int, end: Int): Boolean {
    val arguments =
      Bundle().apply {
        putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, start)
        putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, end)
      }
    return field.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, arguments)
  }

  /** Moves the cursor by `granularity`; with Shift, extends the selection. */
  private fun moveCursor(field: AccessibilityNodeInfo, granularity: Int, forward: Boolean, event: KeyEvent): Boolean {
    val arguments =
      Bundle().apply {
        putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_MOVEMENT_GRANULARITY_INT, granularity)
        putBoolean(
          AccessibilityNodeInfo.ACTION_ARGUMENT_EXTEND_SELECTION_BOOLEAN,
          event.isShiftPressed,
        )
      }
    val action =
      if (forward) AccessibilityNodeInfo.ACTION_NEXT_AT_MOVEMENT_GRANULARITY
      else AccessibilityNodeInfo.ACTION_PREVIOUS_AT_MOVEMENT_GRANULARITY
    return field.performAction(action, arguments)
  }

  /** Deletes the selection, or the character before (or after) the cursor. */
  private fun delete(field: AccessibilityNodeInfo, forward: Boolean): Boolean {
    if (field.isPassword) return false
    val text = text(field)
    var start = field.textSelectionStart
    var end = field.textSelectionEnd
    if (start < 0 || end < 0 || start > text.length || end > text.length) {
      start = text.length
      end = text.length
    }
    if (start > end) start = end.also { end = start }
    if (start == end) {
      if (forward) {
        if (end >= text.length) return true
        end = text.offsetByCodePoints(end, 1)
      } else {
        if (start == 0) return true
        start = text.offsetByCodePoints(start, -1)
      }
    }
    val arguments =
      Bundle().apply {
        putCharSequence(
          AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
          text.substring(0, start) + text.substring(end),
        )
      }
    if (!field.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, arguments)) return false
    return select(field, start, start)
  }
}
