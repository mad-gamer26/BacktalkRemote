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

import org.junit.Assert.assertEquals
import org.junit.Test

/** The volume key presses that stop controlling another phone. */
class RepeatedPressTest {
  private fun presses(vararg times: Long): List<Boolean> {
    val counter = RemoteControl.RepeatedPress(3, 1_500)
    return times.map { counter.press(it) }
  }

  @Test
  fun threeQuickPressesStop() {
    assertEquals(listOf(false, false, true), presses(0, 300, 600))
  }

  @Test
  fun threePressesTooSlowDoNot() {
    assertEquals(listOf(false, false, false), presses(0, 1_000, 2_000))
  }

  @Test
  fun theLastThreeCountAfterASlowStart() {
    assertEquals(listOf(false, false, false, true), presses(0, 5_000, 5_400, 5_800))
  }

  @Test
  fun countingStartsOverAfterStopping() {
    assertEquals(
      listOf(false, false, true, false, false, true),
      presses(0, 100, 200, 300, 400, 500),
    )
  }
}
