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
import android.media.AudioManager
import com.google.android.accessibility.utils.output.LowLatencyAudio
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Hands what Backtalk's low-latency players mix to [send], to go to the controlling devices.
 *
 * The players' audio thread only copies each burst into a ring here and goes on: a thread of this
 * feeder's own passes what has collected to [send] about every [PERIOD_MS], so the native call and
 * the network never hold up Backtalk's audio. When the ring is full, the oldest audio goes, as
 * audio late by a second is no use to the controller.
 */
class RemoteAudioFeeder(context: Context, private val send: (sampleRate: Int, pcm: ByteArray) -> Unit) :
  LowLatencyAudio.Tap {
  private val lock = Any()
  private val ring = FloatArray(RING_FRAMES * 2)
  private var head = 0
  private var count = 0
  @Volatile private var sampleRate: Int =
    context.getSystemService(AudioManager::class.java)
      ?.getProperty(AudioManager.PROPERTY_OUTPUT_SAMPLE_RATE)
      ?.toIntOrNull() ?: DEFAULT_RATE
  @Volatile private var running = false
  private var thread: Thread? = null

  override fun onMixed(frames: FloatArray, sampleRate: Int) {
    synchronized(lock) {
      this.sampleRate = sampleRate
      for (sample in frames) {
        ring[(head + count) % ring.size] = sample
        if (count < ring.size) {
          count++
        } else {
          head = (head + 1) % ring.size
        }
      }
    }
  }

  /** Starts passing audio on, and takes Backtalk's audio, playing it here only if [playHere]. */
  fun start(playHere: Boolean) {
    if (running) {
      LowLatencyAudio.setTap(this, !playHere)
      return
    }
    running = true
    // The stream starts at once, in silence, so the controller hears that the audio works.
    send(sampleRate, ByteArray(0))
    thread =
      Thread(::run, "RemoteAudioFeeder").apply {
        priority = Thread.MAX_PRIORITY
        start()
      }
    LowLatencyAudio.setTap(this, !playHere)
  }

  fun stop() {
    LowLatencyAudio.setTap(null, false)
    running = false
    thread?.interrupt()
    thread = null
    synchronized(lock) {
      head = 0
      count = 0
    }
  }

  private fun run() {
    var block = FloatArray(0)
    var bytes = ByteBuffer.allocate(0)
    while (running) {
      try {
        Thread.sleep(PERIOD_MS)
      } catch (e: InterruptedException) {
        return
      }
      val rate: Int
      val taken: Int
      synchronized(lock) {
        rate = sampleRate
        taken = count - count % 2
        if (block.size < taken) {
          block = FloatArray(taken)
        }
        for (i in 0 until taken) {
          block[i] = ring[(head + i) % ring.size]
        }
        head = (head + taken) % ring.size
        count -= taken
      }
      if (taken == 0) {
        continue
      }
      if (bytes.capacity() < taken * 4) {
        bytes = ByteBuffer.allocate(taken * 4).order(ByteOrder.LITTLE_ENDIAN)
      }
      bytes.clear()
      bytes.asFloatBuffer().put(block, 0, taken)
      val pcm = ByteArray(taken * 4)
      bytes.get(pcm)
      send(rate, pcm)
    }
  }

  private companion object {
    const val DEFAULT_RATE = 48_000
    const val PERIOD_MS = 10L
    /** A second at the fastest rates phones mix at. */
    const val RING_FRAMES = 96_000
  }
}
