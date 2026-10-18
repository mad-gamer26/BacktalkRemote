#!/bin/sh
# Builds the remote access core (remote/core) for iPhone and iPad (device
# and Apple silicon simulator) as Generated/BacktalkRemoteCore.xcframework,
# and generates its Swift bindings into Generated/.
#   remote/ios/build-core.sh
set -eu
# The C parts (libopus, crypto) target the app's oldest iOS (project.yml).
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
out="$here/Generated"
rm -rf "$out"
mkdir -p "$out/include"
cd "$root"
for t in aarch64-apple-ios aarch64-apple-ios-sim; do
    cargo build --release -p backtalk-remote-core --lib --target "$t"
done
# Bindings from the host build of the same crate (identical interface).
cargo build --release -p backtalk-remote-core --lib
cargo run --release -q -p backtalk-remote-core --features bindgen --bin uniffi-bindgen -- \
    generate --library target/release/libbacktalk_remote.dylib --language swift --out-dir "$out"
mv "$out/backtalk_remoteFFI.h" "$out/include/"
mv "$out/backtalk_remoteFFI.modulemap" "$out/include/module.modulemap"
xcodebuild -create-xcframework \
    -library target/aarch64-apple-ios/release/libbacktalk_remote.a -headers "$out/include" \
    -library target/aarch64-apple-ios-sim/release/libbacktalk_remote.a -headers "$out/include" \
    -output "$out/BacktalkRemoteCore.xcframework" >/dev/null
rm -rf "$out/include"
echo "Core and Swift bindings in $out"
