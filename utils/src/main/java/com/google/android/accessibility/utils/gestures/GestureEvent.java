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

package com.google.android.accessibility.utils.gestures;

import android.accessibilityservice.AccessibilityGestureEvent;
import android.os.Build;
import android.view.MotionEvent;
import androidx.annotation.RequiresApi;
import java.util.ArrayList;
import java.util.List;

/**
 * A gesture that {@link GestureManifold} recognized: what Android's {@link
 * AccessibilityGestureEvent} holds, which apps can only make from Android 13. Remote access runs
 * Backtalk's gesture detection on older Android too, for the touches of a controlling device.
 */
public final class GestureEvent {
  private final int gestureId;
  private final int displayId;
  private final List<MotionEvent> motionEvents;

  public GestureEvent(int gestureId, int displayId, List<MotionEvent> motionEvents) {
    this.gestureId = gestureId;
    this.displayId = displayId;
    this.motionEvents = motionEvents;
  }

  /** A gesture with no motion events, such as one Backtalk adds itself. */
  public GestureEvent(int gestureId, int displayId) {
    this(gestureId, displayId, new ArrayList<>());
  }

  public int getGestureId() {
    return gestureId;
  }

  public int getDisplayId() {
    return displayId;
  }

  public List<MotionEvent> getMotionEvents() {
    return motionEvents;
  }

  /** The same gesture as Android's event. */
  @RequiresApi(Build.VERSION_CODES.TIRAMISU)
  public AccessibilityGestureEvent toAccessibilityGestureEvent() {
    return new AccessibilityGestureEvent(gestureId, displayId, motionEvents);
  }
}
