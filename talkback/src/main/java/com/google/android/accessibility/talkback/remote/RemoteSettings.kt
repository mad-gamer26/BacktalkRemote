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

import android.content.SharedPreferences

/**
 * Settings for remote access: whether this phone is shared, its access key, the relay server, and
 * the access key last used to control another phone.
 */
object RemoteSettings {
  /** The official relay. Self-hosters change it in settings; nothing else names it. */
  const val DEFAULT_SERVER = "mad-gamer.com"

  const val PREF_SHARE = "pref_remote_share"
  const val PREF_SERVER = "pref_remote_server"
  const val PREF_SHARE_KEY = "pref_remote_share_key"
  const val PREF_PLAY_HERE = "pref_remote_play_here"
  const val PREF_CONTROL_KEY = "pref_remote_control_key"

  fun isSharing(prefs: SharedPreferences): Boolean = prefs.getBoolean(PREF_SHARE, false)

  fun setSharing(prefs: SharedPreferences, sharing: Boolean) {
    prefs.edit().putBoolean(PREF_SHARE, sharing).apply()
  }

  fun server(prefs: SharedPreferences): String =
    prefs.getString(PREF_SERVER, null)?.trim().takeUnless { it.isNullOrEmpty() } ?: DEFAULT_SERVER

  /** This phone's access key, made the first time it is asked for. */
  fun shareKey(prefs: SharedPreferences, generate: () -> String): String {
    prefs.getString(PREF_SHARE_KEY, null)?.takeIf { it.isNotBlank() }?.let {
      return it
    }
    val key = generate()
    prefs.edit().putString(PREF_SHARE_KEY, key).apply()
    return key
  }

  /** Whether Backtalk still plays on this phone while another device controls it. */
  fun playsHere(prefs: SharedPreferences): Boolean = prefs.getBoolean(PREF_PLAY_HERE, true)

  fun controlKey(prefs: SharedPreferences): String = prefs.getString(PREF_CONTROL_KEY, null) ?: ""
}
