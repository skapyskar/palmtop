#!/usr/bin/env bash
# Build the decode-spike APK without Gradle, using the SDK build-tools directly.
# Avoids pulling a Gradle distribution for what is a single-Activity spike.
set -euo pipefail

source "$HOME/opt/android-env.sh"
cd "$(dirname "$0")"

BT="$ANDROID_HOME/build-tools/36.1.0"
PLATFORM="$ANDROID_HOME/platforms/android-36/android.jar"
OUT=build
rm -rf "$OUT"; mkdir -p "$OUT/classes" "$OUT/dex"

echo "[1/5] aapt2 link (resources + manifest)"
"$BT/aapt2" link \
  -I "$PLATFORM" \
  --manifest AndroidManifest.xml \
  --java "$OUT" \
  --min-sdk-version 30 \
  --target-sdk-version 36 \
  -o "$OUT/base.apk"

echo "[2/5] javac"
javac -source 11 -target 11 -nowarn \
  -classpath "$PLATFORM" \
  -d "$OUT/classes" \
  $(find src -name '*.java') 2>&1 | grep -vE "^(Note:|warning:)" || true

echo "[3/5] d8 (dex)"
"$BT/d8" --lib "$PLATFORM" --min-api 30 --output "$OUT/dex" \
  $(find "$OUT/classes" -name '*.class')

echo "[4/5] package classes.dex into apk"
cp "$OUT/base.apk" "$OUT/unsigned.apk"
(cd "$OUT/dex" && zip -q -X ../unsigned.apk classes.dex)
"$BT/zipalign" -f 4 "$OUT/unsigned.apk" "$OUT/aligned.apk"

echo "[5/5] sign"
if [ ! -f debug.keystore ]; then
  keytool -genkeypair \
    -keystore debug.keystore -storepass android -keypass android \
    -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
    -dname "CN=Palmtop Spike, OU=dev, O=palmtop, L=x, S=x, C=US"
fi
"$BT/apksigner" sign \
  --ks debug.keystore --ks-pass pass:android --key-pass pass:android \
  --ks-key-alias androiddebugkey \
  --out palmtop-spike.apk "$OUT/aligned.apk"

echo "[done] $(ls -la palmtop-spike.apk | awk '{print $5" bytes  "$9}')"
