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
import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.os.Bundle
import android.os.PersistableBundle
import android.view.accessibility.AccessibilityNodeInfo

/** Types text from a controlling device into the focused field of this phone. */
object RemoteTyping {
  enum class Result {
    TYPED,
    NO_FIELD,
    FAILED,
  }

  /**
   * Puts [text] in the field with input focus, at its cursor, in place of any selected text. A
   * password field hides its text, so there the text is pasted instead, through the clipboard.
   */
  fun type(service: AccessibilityService, text: String): Result {
    val field = service.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
    if (field == null || !field.isEditable) {
      return Result.NO_FIELD
    }
    if (field.isPassword) {
      return if (paste(service, field, text)) Result.TYPED else Result.FAILED
    }
    val current = if (field.isShowingHintText) "" else field.text?.toString() ?: ""
    var start = field.textSelectionStart
    var end = field.textSelectionEnd
    if (start < 0 || end < 0 || start > current.length || end > current.length) {
      start = current.length
      end = current.length
    }
    if (start > end) {
      start = end.also { end = start }
    }
    val updated = current.substring(0, start) + text + current.substring(end)
    val setText =
      Bundle().apply {
        putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, updated)
      }
    if (!field.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, setText)) {
      return if (paste(service, field, text)) Result.TYPED else Result.FAILED
    }
    val cursor = start + text.length
    val selection =
      Bundle().apply {
        putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, cursor)
        putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, cursor)
      }
    field.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, selection)
    return Result.TYPED
  }

  private fun paste(service: AccessibilityService, field: AccessibilityNodeInfo, text: String): Boolean {
    val clipboard = service.getSystemService(ClipboardManager::class.java) ?: return false
    val clip = ClipData.newPlainText("", text)
    // Keeps it out of clipboard previews.
    clip.description.extras =
      PersistableBundle().apply { putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true) }
    clipboard.setPrimaryClip(clip)
    return field.performAction(AccessibilityNodeInfo.ACTION_PASTE)
  }
}
