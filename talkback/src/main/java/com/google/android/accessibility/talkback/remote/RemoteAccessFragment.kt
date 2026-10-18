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

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.os.Bundle
import android.text.InputType
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import androidx.preference.CheckBoxPreference
import androidx.preference.EditTextPreference
import androidx.preference.Preference
import androidx.preference.PreferenceCategory
import com.google.android.accessibility.talkback.R
import com.google.android.accessibility.talkback.TalkBackService
import com.google.android.accessibility.talkback.preference.base.TalkbackBaseFragment
import com.google.android.accessibility.utils.SharedPreferencesUtils
import uniffi.backtalk_remote.accessKeyProblem
import uniffi.backtalk_remote.generateAccessKey
import uniffi.backtalk_remote.relayServerProblem

/**
 * Settings for remote access: sharing this phone with its access key, controlling another phone
 * with that phone's key, and the relay server. Status rows say what is happening, in words, as it
 * changes; Backtalk also says the changes that matter as they happen.
 */
class RemoteAccessFragment : TalkbackBaseFragment(), RemoteAccess.Listener {
  private lateinit var prefs: SharedPreferences
  private var shareKey: Preference? = null
  private var shareStatus: Preference? = null
  private var controlStatus: Preference? = null
  private var controlToggle: Preference? = null
  private var audioDetails: Preference? = null

  public override fun getTitle(): CharSequence = getText(R.string.title_pref_remote_access)

  private val remote: RemoteAccess?
    get() = TalkBackService.getInstance()?.remoteAccess

  override fun onCreatePreferences(savedInstanceState: Bundle?, rootKey: String?) {
    prefs = SharedPreferencesUtils.getSharedPreferences(requireContext())
    buildScreen()
  }

  override fun onResume() {
    super.onResume()
    remote?.addListener(this)
    onRemoteStatusChanged()
  }

  override fun onPause() {
    remote?.removeListener(this)
    super.onPause()
  }

  private fun buildScreen() {
    val context = requireContext()
    val screen = preferenceManager.createPreferenceScreen(context)
    val available = RemoteNative.isAvailable(context)

    val share = category(context, R.string.remote_share_category)
    screen.addPreference(share)
    share.addPreference(
      switchPreference(
        context,
        RemoteSettings.PREF_SHARE,
        R.string.pref_remote_share,
        R.string.pref_remote_share_summary,
        RemoteSettings.isSharing(prefs),
      )
    )
    shareStatus = statusPreference(context).also { share.addPreference(it) }
    if (available) {
      val key = RemoteSettings.shareKey(prefs) { generateAccessKey() }
      shareKey =
        textPreference(
            context,
            RemoteSettings.PREF_SHARE_KEY,
            R.string.pref_remote_share_key,
            key,
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD,
          ) { value ->
            accessKeyProblem(value)?.let {
              toast(it)
              false
            }
              ?: run {
                prefs.edit().putString(RemoteSettings.PREF_SHARE_KEY, value.trim()).apply()
                true
              }
          }
          .also { share.addPreference(it) }
      share.addPreference(
        actionPreference(context, R.string.pref_remote_copy_key, null) {
          copyKey(RemoteSettings.shareKey(prefs) { generateAccessKey() })
        }
      )
      share.addPreference(
        actionPreference(context, R.string.pref_remote_share_key_with, null) {
          sendKey(RemoteSettings.shareKey(prefs) { generateAccessKey() })
        }
      )
      share.addPreference(
        actionPreference(context, R.string.pref_remote_new_key, R.string.pref_remote_new_key_summary) {
          val newKey = generateAccessKey()
          prefs.edit().putString(RemoteSettings.PREF_SHARE_KEY, newKey).apply()
          (shareKey as? EditTextPreference)?.text = newKey
          shareKey?.summary = newKey
        }
      )
    }
    share.addPreference(
      switchPreference(
        context,
        RemoteSettings.PREF_PLAY_HERE,
        R.string.pref_remote_play_here,
        R.string.pref_remote_play_here_summary,
        RemoteSettings.playsHere(prefs),
      )
    )

    val control = category(context, R.string.remote_control_category)
    screen.addPreference(control)
    controlStatus = statusPreference(context).also { control.addPreference(it) }
    control.addPreference(
      textPreference(
        context,
        RemoteSettings.PREF_CONTROL_KEY,
        R.string.pref_remote_control_key,
        RemoteSettings.controlKey(prefs),
        inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD,
      ) { value ->
        prefs.edit().putString(RemoteSettings.PREF_CONTROL_KEY, value.trim()).apply()
        true
      }
    )
    controlToggle =
      actionPreference(context, R.string.pref_remote_start_control, R.string.pref_remote_start_control_summary) {
          toggleControl()
        }
        .also { control.addPreference(it) }
    audioDetails =
      actionPreference(context, R.string.pref_remote_audio_details, null) { showAudioDetails() }
        .also { control.addPreference(it) }

    screen.addPreference(
      textPreference(
        context,
        RemoteSettings.PREF_SERVER,
        R.string.pref_remote_server,
        RemoteSettings.server(prefs),
        inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI,
      ) { value ->
        val server = value.trim()
        val problem = if (available) relayServerProblem(server) else null
        if (problem != null) {
          toast(problem)
          false
        } else {
          prefs.edit().putString(RemoteSettings.PREF_SERVER, server).apply()
          true
        }
      }
    )
    preferenceScreen = screen
  }

  override fun onRemoteStatusChanged() {
    if (!isAdded) return
    val remote = remote
    val unavailable =
      if (remote == null) {
        getString(R.string.remote_backtalk_off)
      } else {
        remote.unavailableReason()
      }
    shareStatus?.summary = unavailable ?: remote?.hostText
    val controlling = remote?.isControlling == true
    controlStatus?.summary =
      when {
        remote == null -> getString(R.string.remote_backtalk_off)
        controlling -> remote.controlText
        else -> remote.controlUnavailableReason() ?: getString(R.string.remote_off)
      }
    controlToggle?.apply {
      setTitle(if (controlling) R.string.pref_remote_stop_control else R.string.pref_remote_start_control)
      setSummary(if (controlling) null else getString(R.string.pref_remote_start_control_summary))
    }
    audioDetails?.isVisible = controlling
  }

  private fun toggleControl() {
    val remote = remote
    if (remote == null) {
      toast(getString(R.string.remote_backtalk_off))
      return
    }
    if (remote.isControlling) {
      remote.stopControlling()
    } else {
      remote.startControlling(RemoteSettings.controlKey(prefs))?.let { toast(it) }
    }
    onRemoteStatusChanged()
  }

  private fun showAudioDetails() {
    val details = remote?.audioDetails() ?: getString(R.string.remote_control_audio_waiting)
    AlertDialog.Builder(requireContext())
      .setTitle(R.string.pref_remote_audio_details)
      .setMessage(details)
      .setPositiveButton(android.R.string.ok, null)
      .show()
  }

  private fun copyKey(key: String) {
    val clipboard = requireContext().getSystemService(ClipboardManager::class.java) ?: return
    clipboard.setPrimaryClip(ClipData.newPlainText(getString(R.string.pref_remote_share_key), key))
    toast(getString(R.string.remote_key_copied))
  }

  private fun sendKey(key: String) {
    val send =
      Intent(Intent.ACTION_SEND)
        .setType("text/plain")
        .putExtra(Intent.EXTRA_SUBJECT, getString(R.string.remote_share_key_subject))
        .putExtra(Intent.EXTRA_TEXT, key)
    startActivity(Intent.createChooser(send, getString(R.string.pref_remote_share_key_with)))
  }

  private fun toast(message: String) {
    Toast.makeText(requireContext(), message, Toast.LENGTH_LONG).show()
  }

  private fun category(context: Context, title: Int) =
    PreferenceCategory(context).apply {
      setTitle(title)
      isIconSpaceReserved = false
    }

  /** A row that only says the state; screen readers read it like any other row. */
  private fun statusPreference(context: Context) =
    Preference(context).apply {
      isPersistent = false
      layoutResource = R.layout.listitem_2texts
      setTitle(R.string.pref_remote_status)
      isSelectable = false
    }

  private fun switchPreference(
    context: Context,
    key: String,
    title: Int,
    summary: Int?,
    checked: Boolean,
  ) =
    CheckBoxPreference(context).apply {
      this.key = key
      isPersistent = false
      layoutResource = R.layout.listitem_2texts
      setTitle(title)
      summary?.let { setSummary(it) }
      isChecked = checked
      setOnPreferenceChangeListener { _, newValue ->
        prefs.edit().putBoolean(key, newValue as Boolean).apply()
        true
      }
    }

  private fun actionPreference(context: Context, title: Int, summary: Int?, onClick: () -> Unit) =
    Preference(context).apply {
      isPersistent = false
      layoutResource = R.layout.listitem_2texts
      setTitle(title)
      summary?.let { setSummary(it) }
      setOnPreferenceClickListener {
        onClick()
        true
      }
    }

  /**
   * A text setting edited in a dialog; its summary is the text. [save] returns whether the text
   * was taken.
   */
  private fun textPreference(
    context: Context,
    key: String,
    title: Int,
    value: String,
    inputType: Int,
    save: (String) -> Boolean,
  ): Preference =
    EditTextPreference(context).apply {
      // The edit dialog finds its preference by key. Saving is up to [save].
      this.key = key
      isPersistent = false
      layoutResource = R.layout.listitem_2texts
      setTitle(title)
      dialogTitle = getString(title)
      text = value
      summary = value.ifEmpty { getString(R.string.remote_not_set) }
      setOnBindEditTextListener {
        it.inputType = inputType
        it.setSelection(it.text.length)
      }
      setOnPreferenceChangeListener { preference, newValue ->
        val text = newValue as String
        val taken = save(text)
        if (taken) {
          preference.summary = text.trim().ifEmpty { getString(R.string.remote_not_set) }
        }
        taken
      }
    }
}
