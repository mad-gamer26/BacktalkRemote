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

import android.view.KeyEvent
import com.google.android.accessibility.talkback.Feedback
import com.google.android.accessibility.talkback.TalkBackService
import com.google.android.accessibility.utils.traversal.TraversalStrategy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.backtalk_remote.KeyAction
import uniffi.backtalk_remote.KeyInput

/**
 * Keys from a controlling device's keyboard: Backtalk gets each first, as a physical keyboard's,
 * and the keys it does not take do what they do in an app. No text field has focus here.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RemoteKeyboardTest {
  /** Takes the keys a test says Backtalk's shortcuts take, and records every key it sees. */
  class KeyService : TalkBackService() {
    val seen = mutableListOf<String>()
    val shortcuts = mutableSetOf<Int>()

    override fun onKeyEventInternal(keyEvent: KeyEvent): Boolean {
      val action = if (keyEvent.action == KeyEvent.ACTION_DOWN) "down" else "up"
      seen.add("$action ${keyEvent.keyCode} ${keyEvent.repeatCount}")
      return keyEvent.keyCode in shortcuts && keyEvent.isAltPressed
    }
  }

  private lateinit var service: KeyService
  private lateinit var keyboard: RemoteKeyboard
  private val feedback = mutableListOf<Feedback.Part>()

  @Before
  fun setUp() {
    service = Robolectric.buildService(KeyService::class.java).get()
    keyboard =
      RemoteKeyboard(service) { f ->
        feedback.addAll(f.failovers())
        true
      }
  }

  private fun key(action: KeyAction, code: Int, meta: Int = 0, repeat: Int = 0) =
    keyboard.onKey(KeyInput(action, code.toUInt(), meta.toUInt(), repeat.toUInt()))

  private fun press(code: Int, meta: Int = 0) {
    key(KeyAction.DOWN, code, meta)
    key(KeyAction.UP, code, meta)
  }

  private fun directions() = feedback.mapNotNull { it.focusDirection()?.direction() }

  @Test
  fun backtalkShortcutsGoToBacktalkOnly() {
    service.shortcuts.add(KeyEvent.KEYCODE_DPAD_RIGHT)
    val alt = KeyEvent.META_ALT_ON or KeyEvent.META_ALT_LEFT_ON
    key(KeyAction.DOWN, KeyEvent.KEYCODE_ALT_LEFT, alt)
    press(KeyEvent.KEYCODE_DPAD_RIGHT, alt)
    key(KeyAction.UP, KeyEvent.KEYCODE_ALT_LEFT)
    assertEquals(
      listOf("down ${KeyEvent.KEYCODE_ALT_LEFT} 0", "down ${KeyEvent.KEYCODE_DPAD_RIGHT} 0", "up ${KeyEvent.KEYCODE_DPAD_RIGHT} 0", "up ${KeyEvent.KEYCODE_ALT_LEFT} 0"),
      service.seen,
    )
    // Backtalk took the arrow, so it did not also move focus.
    assertTrue(feedback.isEmpty())
  }

  @Test
  fun keysBacktalkLeavesMoveFocusAndClick() {
    press(KeyEvent.KEYCODE_DPAD_DOWN)
    press(KeyEvent.KEYCODE_TAB)
    press(KeyEvent.KEYCODE_TAB, KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON)
    assertEquals(
      listOf(
        TraversalStrategy.SEARCH_FOCUS_DOWN,
        TraversalStrategy.SEARCH_FOCUS_FORWARD,
        TraversalStrategy.SEARCH_FOCUS_BACKWARD,
      ),
      directions(),
    )
    feedback.clear()
    press(KeyEvent.KEYCODE_ENTER)
    assertEquals(Feedback.Focus.Action.CLICK_CURRENT, feedback.single().focus()?.action())
    // Every key went to Backtalk first, down and up.
    assertEquals(8, service.seen.size)
  }

  @Test
  fun aHeldKeyRepeatsUntilItsUp() {
    key(KeyAction.DOWN, KeyEvent.KEYCODE_DPAD_LEFT)
    key(KeyAction.DOWN, KeyEvent.KEYCODE_DPAD_LEFT, repeat = 1)
    key(KeyAction.DOWN, KeyEvent.KEYCODE_DPAD_LEFT, repeat = 2)
    key(KeyAction.UP, KeyEvent.KEYCODE_DPAD_LEFT)
    assertEquals(List(3) { TraversalStrategy.SEARCH_FOCUS_LEFT }, directions())
    assertEquals(
      listOf("down ${KeyEvent.KEYCODE_DPAD_LEFT} 0", "down ${KeyEvent.KEYCODE_DPAD_LEFT} 1", "down ${KeyEvent.KEYCODE_DPAD_LEFT} 2", "up ${KeyEvent.KEYCODE_DPAD_LEFT} 0"),
      service.seen,
    )
  }

  @Test
  fun keysStillDownAreLetGoWhenTheControllerLeaves() {
    key(KeyAction.DOWN, KeyEvent.KEYCODE_SHIFT_LEFT, KeyEvent.META_SHIFT_ON)
    keyboard.releaseAll()
    assertEquals(listOf("down ${KeyEvent.KEYCODE_SHIFT_LEFT} 0", "up ${KeyEvent.KEYCODE_SHIFT_LEFT} 0"), service.seen)
    // An up without its down goes nowhere.
    key(KeyAction.UP, KeyEvent.KEYCODE_SHIFT_LEFT)
    assertEquals(2, service.seen.size)
  }
}
