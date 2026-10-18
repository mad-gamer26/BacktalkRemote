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
import com.google.android.libraries.accessibility.utils.log.LogUtils

/**
 * The native remote access core (remote/core, in Rust): relay connection, keys, and AudioNet's
 * audio transport. Its Kotlin bindings, in package uniffi.backtalk_remote, load it through JNA; this
 * loads it first and hands it the app's context, which its audio output needs.
 */
object RemoteNative {
  private const val TAG = "RemoteNative"
  private const val LIBRARY = "backtalk_remote"

  @Volatile private var loaded: Boolean? = null

  /**
   * Whether the core is in this build for this phone's processor, loading it the first time. A
   * build made without Rust has no core, and remote access says so rather than failing.
   */
  @JvmStatic
  fun isAvailable(context: Context): Boolean {
    loaded?.let {
      return it
    }
    synchronized(this) {
      loaded?.let {
        return it
      }
      val ok =
        try {
          System.loadLibrary(LIBRARY)
          attach(context.applicationContext)
          true
        } catch (e: UnsatisfiedLinkError) {
          LogUtils.e(TAG, "The remote access core is not in this build: %s", e)
          false
        }
      loaded = ok
      return ok
    }
  }

  @JvmStatic private external fun attach(context: Context)
}
