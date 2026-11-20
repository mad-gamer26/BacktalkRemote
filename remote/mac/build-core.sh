#!/bin/sh
# Builds the remote access core (remote/core) for the Mac (Apple silicon
# and Intel, one universal library) as Generated/BacktalkRemoteCore.xcframework,
# and generates its Swift bindings into Generated/.
#   remote/mac/build-core.sh
set -eu
# The C parts (libopus, crypto) target the app's oldest macOS (project.yml).
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
out="$here/Generated"
rm -rf "$out"
mkdir -p "$out/include" "$out/lib"
cd "$root"
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release -p backtalk-remote-core --lib --target "$t"
done
lipo -create \
    target/aarch64-apple-darwin/release/libbacktalk_remote.a \
    target/x86_64-apple-darwin/release/libbacktalk_remote.a \
    -output "$out/lib/libbacktalk_remote.a"
# Bindings from the host build of the same crate (identical interface).
cargo build --release -p backtalk-remote-core --lib
cargo run --release -q -p backtalk-remote-core --features bindgen --bin uniffi-bindgen -- \
    generate --library target/release/libbacktalk_remote.dylib --language swift --out-dir "$out"
mv "$out/backtalk_remoteFFI.h" "$out/include/"
mv "$out/backtalk_remoteFFI.modulemap" "$out/include/module.modulemap"
xcodebuild -create-xcframework \
    -library "$out/lib/libbacktalk_remote.a" -headers "$out/include" \
    -output "$out/BacktalkRemoteCore.xcframework" >/dev/null
rm -rf "$out/include" "$out/lib"
echo "Core and Swift bindings in $out"
