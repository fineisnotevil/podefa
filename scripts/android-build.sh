#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
#
# Builds the Android APK (Plan 0008 Phase 1, ADR 0012): target/android/podefa.apk, arm64.
#
#   scripts/android-build.sh [--debug]
#
# Needs: ANDROID_HOME (SDK with a platform and build-tools 35+), ANDROID_NDK_HOME (NDK r28+),
# a JDK (javac, keytool) on PATH or in JAVA_HOME, cargo-apk 0.10, and the Rust target
# aarch64-linux-android.
#
# Signing (Plan 0008 Phase 5): with PODEFA_KEYSTORE set to a release keystore, the APK is signed
# with its key (PODEFA_KEY_ALIAS picks one when it holds several) and apksigner asks for the
# keystore's password on the terminal: no password goes through the environment or a file.
# Without it, the APK is signed with a throwaway debug key the script makes in target/android
# (fine for testing, not for publishing; CI builds that way). docs/android-release.md has the
# steps for a release key.
#
# Three steps cargo-apk cannot do alone:
#  1. MuPDF's bundled FreeType and libjpeg clash with Skia's (Plan 0008, §3.3): the build uses a
#     copy of mupdf-sys with patches/mupdf-sys-android.patch, patched in through a Cargo config
#     in target/android, so the desktop build and Cargo.lock stay as they are.
#  2. The activity's Java class (crates/android/java) is compiled to classes.dex and added to
#     the APK, which is then aligned and signed again.
#  3. The C++ runtime is linked by build scripts (crates/android, crates/engine-mupdf), not by
#     the environment.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work="$root/target/android"
profile=release
profile_flag=--release
if [[ "${1:-}" == "--debug" ]]; then
    profile=debug
    profile_flag=
fi

: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
ndk="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
[[ -n "$ndk" ]] || { echo "set ANDROID_NDK_HOME to the NDK" >&2; exit 1; }
export ANDROID_NDK_HOME="$ndk" ANDROID_NDK_ROOT="$ndk"
if [[ -n "${JAVA_HOME:-}" ]]; then
    export PATH="$JAVA_HOME/bin:$PATH"
fi

llvm_bin=$(ls -d "$ndk"/toolchains/llvm/prebuilt/*/bin | head -n 1)
build_tools=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -n 1)
android_jar=$(ls "$ANDROID_HOME"/platforms/android-*/android.jar | sort -V | tail -n 1)
export FA_PDF_LLVM_BIN="$llvm_bin"
# mupdf-sys runs bindgen for the target: the NDK's own libclang, sysroot and compiler headers.
# The NDK's libclang does not find its own headers (lib/clang/<version>): without
# -resource-dir it fails on stddef.h, or, where the host has a clang of its own (CI runners),
# mixes the host's headers with the NDK's and fails on uint32_t.
toolchain=$(dirname "$llvm_bin")
resource_dir=$(ls -d "$toolchain"/lib/clang/* | sort -V | tail -n 1)
export LIBCLANG_PATH="$toolchain/lib"
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android="--sysroot=$toolchain/sysroot -resource-dir $resource_dir"
echo "bindgen: LIBCLANG_PATH=$LIBCLANG_PATH args=$BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android"

mkdir -p "$work"

# 1. The patched mupdf-sys, from the registry's copy.
cargo fetch --manifest-path "$root/Cargo.toml" >/dev/null
sys_manifest=$(cargo metadata --format-version 1 --manifest-path "$root/Cargo.toml" |
    grep -o '"manifest_path":"[^"]*/mupdf-sys-[0-9.]*/Cargo.toml"' | head -n 1 | cut -d'"' -f4)
[[ -n "$sys_manifest" ]] || { echo "mupdf-sys not found in cargo metadata" >&2; exit 1; }
sys_src=$(dirname "$sys_manifest")
sys_copy="$work/mupdf-sys"
stamp="$sys_copy/.podefa-patched"
if [[ ! -f "$stamp" ]] || ! cmp -s "$stamp" "$root/patches/mupdf-sys-android.patch"; then
    rm -rf "$sys_copy"
    cp -R "$sys_src" "$sys_copy"
    chmod -R u+w "$sys_copy"
    patch -d "$sys_copy" -p1 < "$root/patches/mupdf-sys-android.patch"
    cp "$root/patches/mupdf-sys-android.patch" "$stamp"
fi
mkdir -p "$work/.cargo"
cat > "$work/.cargo/config.toml" <<EOF
# Written by scripts/android-build.sh: only builds started in target/android see it.
[patch.crates-io]
mupdf-sys = { path = "$sys_copy" }
EOF

# The patch changes mupdf-sys's source in the lock file; the build gets its own copy of the lock.
lock_backup="$work/Cargo.lock.desktop"
cp "$root/Cargo.lock" "$lock_backup"
restore_lock() { cp "$lock_backup" "$root/Cargo.lock"; }
trap restore_lock EXIT

# Signing. cargo-apk always signs with the throwaway debug key; the last step signs again, with
# the release key when one is given.
debug_keystore="$work/debug.keystore"
debug_password=android
if [[ ! -f "$debug_keystore" ]]; then
    keytool -genkeypair -keystore "$debug_keystore" -storepass "$debug_password" \
        -keypass "$debug_password" -alias androiddebugkey -keyalg RSA -keysize 2048 \
        -validity 10000 -dname "CN=PODEFA debug" >/dev/null
fi
release_keystore="${PODEFA_KEYSTORE:-}"
if [[ -n "$release_keystore" && ! -f "$release_keystore" ]]; then
    echo "PODEFA_KEYSTORE: no keystore at $release_keystore" >&2
    exit 1
fi
profile_upper=$(echo "$profile" | tr '[:lower:]' '[:upper:]')
export "CARGO_APK_${profile_upper}_KEYSTORE=$debug_keystore"
export "CARGO_APK_${profile_upper}_KEYSTORE_PASSWORD=$debug_password"

(cd "$work" && cargo apk build --manifest-path "$root/crates/android/Cargo.toml" --lib $profile_flag)

# 2. The activity's dex, added to the APK cargo-apk left unaligned; then align and sign again.
classes="$work/classes"
rm -rf "$classes" "$work/dex"
mkdir -p "$classes" "$work/dex"
javac -source 8 -target 8 -Xlint:-options -encoding UTF-8 -classpath "$android_jar" \
    -d "$classes" "$root"/crates/android/java/org/fineassociation/podefa/*.java
"$build_tools/d8" --min-api 26 $([[ $profile == release ]] && echo --release) \
    --lib "$android_jar" --output "$work/dex" $(find "$classes" -name '*.class')

apk_dir="$root/target/$profile/apk"
unaligned="$apk_dir/podefa-unaligned.apk"
cp "$work/dex/classes.dex" "$apk_dir/classes.dex"
(cd "$apk_dir" && "$build_tools/aapt" remove podefa-unaligned.apk classes.dex >/dev/null 2>&1 || true)
(cd "$apk_dir" && "$build_tools/aapt" add podefa-unaligned.apk classes.dex >/dev/null)
"$build_tools/zipalign" -f 4 "$unaligned" "$work/podefa.apk"
if [[ -n "$release_keystore" ]]; then
    # No --ks-pass: apksigner reads the password from the terminal.
    alias_args=()
    if [[ -n "${PODEFA_KEY_ALIAS:-}" ]]; then
        alias_args=(--ks-key-alias "$PODEFA_KEY_ALIAS")
    fi
    "$build_tools/apksigner" sign --ks "$release_keystore" "${alias_args[@]}" "$work/podefa.apk"
    signed_with="the release key in $release_keystore"
else
    "$build_tools/apksigner" sign --ks "$debug_keystore" --ks-pass "pass:$debug_password" \
        "$work/podefa.apk"
    signed_with="the throwaway debug key"
fi
"$build_tools/apksigner" verify "$work/podefa.apk"

echo "built $work/podefa.apk ($(du -h "$work/podefa.apk" | cut -f1)), signed with $signed_with"
