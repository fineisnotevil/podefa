<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Signing a release APK

`scripts/android-build.sh` signs every APK. Without a release key it uses a throwaway debug key
it makes in `target/android`; that is how CI and the phone tests build, and such an APK is not for
publishing. A release APK is signed with the project's release key, which only its keeper holds
(Plan 0008 Phase 5).

Android installs an update only when it carries the same key as the installed app. A lost
release key, or a lost password for it, means no update can be installed over a release made with
it: users would have to uninstall first. Keep the keystore and its password apart, each with a
backup, and never in this repository.

## Making the release key (once)

On the machine that builds releases, with a JDK on `PATH`:

```sh
keytool -genkeypair -keystore ~/keys/podefa-release.jks -alias podefa \
    -keyalg RSA -keysize 4096 -validity 10000
```

`keytool` asks for the keystore's password and the name to put in the certificate. Give the key
the same password as the keystore (`apksigner` then asks once).

## Building a signed release

```sh
PODEFA_KEYSTORE=~/keys/podefa-release.jks scripts/android-build.sh
```

The script builds as always, then `apksigner` asks for the keystore's password on the terminal.
No password goes through the environment, the command line or a file. `PODEFA_KEY_ALIAS` picks
the key when the keystore holds more than one. The script checks the signature at the end
(`apksigner verify`) and says which key it used.

To see the certificate an APK carries (its SHA-256 digest identifies the key):

```sh
"$ANDROID_HOME"/build-tools/<version>/apksigner verify --print-certs target/android/podefa.apk
```

## Versions

cargo-apk takes the version from the workspace's `version` in `Cargo.toml`: `versionName` is the
version itself, `versionCode` packs its numbers into bytes, `1 << 24 | major << 16 | minor << 8 |
patch` (0.1.0 gives 16777472; `ndk-build-0.10.0/src/cargo.rs:146-151`). Android refuses an
update whose `versionCode` is not higher than the installed one's, so raise the version for every
release.
