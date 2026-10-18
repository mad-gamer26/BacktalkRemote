# Remote access

Backtalk Remote lets another device control an Android phone running Backtalk Remote, and hear the phone's Backtalk. The controlling device can be an iPhone, with the Backtalk Remote app, or another Android phone running Backtalk Remote. Its touch screen becomes the phone's: touches go to the phone as they happen, and the phone's Backtalk handles them exactly as it handles its own touch screen, with the same gestures, the same timing and the same settings. What the phone's Backtalk says and plays comes back to the controlling device.

The two devices meet through a relay server. The official one is `mad-gamer.com`, which the apps fill in; you can run your own.

## Sharing a phone

1.  On the phone, open Backtalk settings, then **Remote access**.
2.  Turn on **Allow remote control**. The phone connects to the relay server, and Backtalk says "This phone can be controlled remotely".
3.  The **Access key** row shows the phone's access key, such as `kq7m-2xpr-9tdf-h4wa`. Give it to the person or device that will control the phone: **Copy access key** and **Send access key** help. Anyone with the key can control the phone, so treat it like a password. **Make a new access key** replaces it; devices using the old one can no longer connect. You can also type a key of your own, at least 8 characters.

While a device controls the phone, Backtalk says so ("Matthew's iPhone is controlling this phone"), a notification shows who is connected and has a **Stop sharing** action, and the screen stays on. When the device leaves, Backtalk says that too.

**Play Backtalk on this phone while controlled** is on by default, so the phone still speaks. Turn it off to hear Backtalk only on the controlling device.

Sharing a phone needs Android 11 or later. On Android 13 and later, remote touches go through the same gesture handling as the phone's touch screen, as described under How it works. On Android 11 and 12, where Android rather than Backtalk recognizes the touch screen's gestures, Backtalk recognizes the remote touches itself, with the gesture detection it uses on Android 13; a double tap then clicks the item, as Android's does, rather than following the double tap setting in Backtalk's gesture settings.

## Controlling from an iPhone

1.  Install the Backtalk Remote iPhone app.
2.  Enter the phone's access key. Capital letters count. The relay server is `mad-gamer.com` unless you run your own.
3.  Choose **Connect**.

Once connected, VoiceOver says "Controlling Pixel 8. The lower part of the screen is now the phone's screen." The top of the screen has the status and buttons that work with VoiceOver as usual: **Disconnect**, **Type text**, **Audio details**, and **Back**, **Home**, **Recent apps**, **Notifications** and **Quick settings**. Below them is the phone's screen. VoiceOver passes touches there straight to the phone, so use the phone's Backtalk gestures there, not VoiceOver's. To get back to the buttons, touch the top of the screen. VoiceOver's escape gesture (a two-finger scrub) disconnects.

**Type text** sends text to the text field that has focus on the phone, at its cursor. That is quicker than the phone's on-screen keyboard. In a password field, the text goes through the phone's clipboard.

The iPhone stays awake while it controls the phone, and the phone's audio keeps playing if you switch to another app.

## Controlling from another Android phone

1.  On the controlling phone, open Backtalk settings, then **Remote access**.
2.  Under **Control another phone**, enter the other phone's access key in **Access key of the other phone**.
3.  Choose **Start controlling**.

Once connected, Backtalk says "Controlling Pixel 8. To stop, press volume down three times." From the next touch on, every touch on this phone's screen goes to the other phone, and its Backtalk is heard here. Touches only go once connected, so this phone works as usual while it waits.

To stop, press volume down three times quickly (the key still changes the volume), choose the notification's stop action, or **Stop controlling** in settings.

Controlling needs Android 13 or later and **Handle gestures in Backtalk**, in advanced settings, which is on by default: Backtalk then gets the raw touch, which it can send on.

## How it works

### Touch

The controlling device sends each change to its touch as it happens: a finger down, the fingers moving, a finger up, with every finger's position from 0 to 1 across and down its touch area, and the time on its own clock. The phone rebuilds Android motion events from them, spread over its whole screen, and plays them with the timing they had, shifted by the network's delay plus 40 ms to absorb its changes. One device touches at a time.

On the phone, the touches go to a second copy of the code Backtalk uses for its own touch screen (`TouchInteractionMonitor`), with Backtalk's gesture detection, timeouts, lift to type, rotation, the circle menu that follows a held touch, and every gesture setting. Where Android's touch explorer would act on the touch screen, a remote stand-in does the same (`RemoteTouchController`, behind the `TouchController` interface):

*   **Touch exploration**: the item under the finger is found in the window at that point and sent to Backtalk as a hover would be, so Backtalk focuses and speaks it as usual.
*   **Interaction start and end**: Backtalk gets the same events as for the touch screen, so speech, hints and the volume keys react as usual. An interaction ends a double tap's time after the last finger lifts, as on the touch screen, so multi-tap gestures stay one interaction.
*   **Double tap**: Backtalk handles it as usual.
*   **Passing the touch to the app** (two-finger scrolling and dragging, double tap and hold): Android lets a service inject a touch only as a whole gesture, so the touch is recorded and played to the app when the fingers lift. One finger plays with its own timing, so a hold stays a hold; several play each at an even speed. While a touch plays, the whole screen passes touch through, and direct touch's region comes back after.

Differences from the phone's own touch screen: a touch passed to the app plays when the fingers lift rather than as they move, and several fingers passed to the app move at an even speed. Touch exploration finds items by their bounds, which is how Android's hover usually ends up too, but a view that handles hover in its own way may differ.

### Audio

The phone sends what Backtalk's low-latency player mixes: its speech, sounds, and 3D sounds. While a device controls the phone, speech and sounds go through that player even if **Low-latency audio** is off, and a sound's first play waits for it to be decoded rather than playing the usual way. What still cannot be heard remotely: speech and sounds while an app records from the microphone or during a phone call (Backtalk plays them the usual way then), and the keyboard's typing sounds.

The audio travels with AudioNet's transport ([github.com/mad-gamer26/AudioNet](https://github.com/mad-gamer26/AudioNet)): 48 kHz stereo Opus in 10 ms frames, RTP framing, XChaCha20-Poly1305 encryption of each packet, and on the controlling device AudioNet's receiver (reordering, loss concealment) and adaptive playout buffer with clock-drift and depth control, starting at 60 ms. The relay copies the phone's UDP packets to each controlling device.

The player's timing is not real time: it runs only while there is something to play, and writes ahead in bursts. A reclock on the phone hands AudioNet exactly the audio due by the clock: bursts wait in a short queue, and silence fills in only when the queue has been empty for 30 ms. If the player's clock runs slower than the phone's, a long stretch of continuous sound without a pause (minutes) can get one 30 ms gap.

### Security

The access key is never sent anywhere. Both devices stretch it with Argon2id, then derive with HKDF:

*   the **room** name, the only thing the relay sees, so devices with the same key find each other;
*   the **handshake** key, which seals the controlling device's hello and the phone's welcome;
*   each connection's **session** key, from both sides' random nonces, which seals touches, actions and text;
*   each audio stream's **media** key, for AudioNet's packet encryption.

Every message between the devices is sealed with XChaCha20-Poly1305 and a random nonce, authenticated with its kind and direction, and carries a sequence number that only goes up, so the relay cannot read, change, reflect or replay it. A wrong key is another room, so a device with a wrong key finds no phone. Each guess at a key costs an attacker an Argon2id stretch; generated keys have about 79 bits.

What the relay does see: who connects, from which address, when, to which room (a hash), and how much they send.

## Relay server

The relay (`remote/relay`) is a small Rust program. It holds no keys and keeps nothing on disk: a room exists while someone is in it. Apps connect to it with a WebSocket (`/backtalk-remote/ws`) for joining and messages, and with UDP for audio. It limits connections per address, controllers per room (8), messages per second, and packets per second.

To run your own:

1.  Build it: `cargo build --release -p backtalk-remote-relay` in `remote/`.
2.  Run it with `--listen 127.0.0.1:39860 --udp 0.0.0.0:39861`, for example with `remote/deploy/systemd/backtalk-remote-relay.service`.
3.  Put a TLS proxy in front of the WebSocket: `remote/deploy/nginx/backtalk-remote.conf` goes in the nginx server block of your host name.
4.  Open UDP port 39861 in your firewall. If a firewall maps another public port, tell the relay with `--udp-public-port`.
5.  In the apps, enter your host name as the relay server. `https://your.host/backtalk-remote/health` says whether it runs.

A relay server setting can be a host name (`relay.example.org`), a host and port (`relay.example.org:8443`), both over TLS, or a whole `wss://` URL; `ws://` works for testing on a local network.

The official relay is deployed with `remote/deploy/official/deploy.sh`.

## Building

The native part (`remote/core`, Rust) is built by Gradle with the rest of Backtalk, with `cargo ndk`, and its Kotlin bindings are generated with UniFFI. You need Rust with the targets `aarch64-linux-android`, `armv7-linux-androideabi` and `x86_64-linux-android`, and `cargo install cargo-ndk`. Without them, the build fails at `buildRemoteCore`.

The iPhone app is in `remote/ios`. On a Mac with Xcode, Rust with the targets `aarch64-apple-ios` and `aarch64-apple-ios-sim`, and XcodeGen:

```sh
remote/ios/build-core.sh
cd remote/ios && xcodegen generate
xcodebuild -project BacktalkRemote.xcodeproj -scheme BacktalkRemote \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' test
```

To run on an iPhone, add `DEVELOPMENT_TEAM=<your team> -allowProvisioningUpdates`.

## Releases

Backtalk Remote is published on GitHub. Each push to `master` updates the `dev` pre-release; a tag such as `v1.0.1` updates the `stable` release. Builds from either follow their own release for updates (`-PupdateChannel=stable` for release builds; `dev` is the default). Both are signed with Backtalk Remote's release key, which the repository holds as the secrets `BACKTALK_REMOTE_KEYSTORE_BASE64`, `BACKTALK_REMOTE_KEYSTORE_PASSWORD`, `BACKTALK_REMOTE_KEY_ALIAS` and `BACKTALK_REMOTE_KEY_PASSWORD`. Android only installs an update signed with the same key, so the key must never change or be lost. Without it (a fork, a pull request), builds are signed with a debug key.

To make a release, tag the commit and push the tag:

```sh
git tag v1.0.1
git push origin v1.0.1
```

To sign a build on your own computer, add `keystore.file`, `keystore.password`, `key.alias` and `key.password` to `local.properties`.

The iPhone app is not published yet; build it as above.

## Testing

`cargo test` in `remote/` tests the protocol, the relay, keys and sealing, the reclock, and a phone and a controlling device through a relay on the same computer, audio included. `remote-probe` (`cargo run --release --bin remote-probe -- --server mad-gamer.com --key KEY`) is a controlling device without a screen for testing: it sends scripted gestures to a phone and records its audio.
