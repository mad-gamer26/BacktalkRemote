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

package com.google.android.accessibility.talkback;

import android.accessibilityservice.TouchInteractionController;
import android.os.Build;
import androidx.annotation.RequiresApi;

/**
 * What {@link TouchInteractionMonitor} asks of the touch it watches: the parts of Android's {@link
 * TouchInteractionController} it uses. Android's controller handles the touch screen; remote
 * access has its own, for touches that come from a controlling device, so that Backtalk treats them
 * exactly as it treats its own touch screen.
 */
public interface TouchController {
  /** The most fingers a touch can have. Pointer IDs are below this. */
  int getMaxPointerCount();

  /** The state now, one of {@link TouchInteractionController}'s {@code STATE_} constants. */
  int getState();

  /** Clicks the item with accessibility focus. */
  void performClick();

  /** Long clicks the item with accessibility focus, and drags it if the touch moves. */
  void performLongClickAndStartDrag();

  /** Explores the screen by touch for the rest of this touch. */
  void requestTouchExploration();

  /** Passes the rest of this touch to the app as one finger, {@code pointerId}, dragging. */
  void requestDragging(int pointerId);

  /** Passes the rest of this touch to the app as it is. */
  void requestDelegating();

  /** Android's controller for the touch screen of one display. */
  @RequiresApi(Build.VERSION_CODES.TIRAMISU)
  static TouchController of(TouchInteractionController controller) {
    return new TouchController() {
      @Override
      public int getMaxPointerCount() {
        return controller.getMaxPointerCount();
      }

      @Override
      public int getState() {
        return controller.getState();
      }

      @Override
      public void performClick() {
        controller.performClick();
      }

      @Override
      public void performLongClickAndStartDrag() {
        controller.performLongClickAndStartDrag();
      }

      @Override
      public void requestTouchExploration() {
        controller.requestTouchExploration();
      }

      @Override
      public void requestDragging(int pointerId) {
        controller.requestDragging(pointerId);
      }

      @Override
      public void requestDelegating() {
        controller.requestDelegating();
      }
    };
  }
}
