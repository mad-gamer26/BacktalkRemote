//! `RemoteNative.attach(context)` (Kotlin, package
//! `com.google.android.accessibility.talkback.remote`): hands the
//! application context to cpal's AAudio backend, which needs it to open
//! the audio output, as AudioNet's `audionet-android` does for AudioNet.
//!
//! Called once on the app's main thread before a controller session
//! starts. The context is kept as a global reference for the life of the
//! process (the application context, never an activity).

use std::ffi::c_void;
use std::sync::OnceLock;

use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{Global, JClass, JObject};

/// The application context, kept for the life of the process:
/// `ndk-context` holds only its raw pointer.
static CONTEXT: OnceLock<Global<JObject<'static>>> = OnceLock::new();

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_google_android_accessibility_talkback_remote_RemoteNative_attach<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            if CONTEXT.get().is_some() {
                return Ok(());
            }
            let vm = env.get_java_vm()?;
            let global = env.new_global_ref(&context)?;
            let raw_context = global.as_raw();
            if CONTEXT.set(global).is_err() {
                return Ok(());
            }
            // SAFETY: `vm` is this process's JavaVM, valid for its whole
            // life; `raw_context` is a global reference kept alive in
            // `CONTEXT` for the life of the process. This runs once.
            unsafe {
                ndk_context::initialize_android_context(
                    vm.get_raw().cast::<c_void>(),
                    raw_context.cast::<c_void>(),
                );
            }
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>();
}
