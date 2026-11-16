#!/bin/bash
# Experimental native bundle; no Apple signing identity or notarization needed.
set -euo pipefail
cd "$(dirname "$0")/../.."
test "$(uname -s)" = Darwin
cargo +1.96.1 build --locked --release -p taskmanager-app --no-default-features --features wgpu,production-logging
cargo +1.96.1 build --locked --release --manifest-path subprojects/magpie/Cargo.toml -p magpie
bundle='target/macos/Task Manager.app'
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp target/release/taskmanager "$bundle/Contents/MacOS/taskmanager"
cp subprojects/magpie/target/release/magpie "$bundle/Contents/MacOS/missioncenter-magpie"
cp COPYING "$bundle/Contents/Resources/COPYING"
cp docs/PROVENANCE.md "$bundle/Contents/Resources/PROVENANCE.md"
iconset='target/macos/TaskManager.iconset'
mkdir -p "$iconset"
for size in 16 32 128 256; do
  cp "data/icons/hicolor/${size}x${size}/apps/io.github.taskmanager.TaskManager.png" "$iconset/icon_${size}x${size}.png"
done
# Use only the project's user-provided icon, never Apple/Microsoft system assets.
iconutil -c icns "$iconset" -o "$bundle/Contents/Resources/TaskManager.icns"
if command -v msgfmt >/dev/null 2>&1; then
  for po in po/*.po; do
    language="$(basename "$po" .po)"
    mkdir -p "$bundle/Contents/Resources/locale/$language/LC_MESSAGES"
    msgfmt "$po" -o "$bundle/Contents/Resources/locale/$language/LC_MESSAGES/taskmanager.mo"
  done
fi
python3 - "$bundle" <<'PY'
import pathlib, plistlib, sys
bundle = pathlib.Path(sys.argv[1])
with (bundle / 'Contents/Info.plist').open('wb') as f:
    plistlib.dump({
        'CFBundleName': 'Task Manager',
        'CFBundleDisplayName': 'Task Manager',
        'CFBundleIdentifier': 'io.github.taskmanager.TaskManager',
        'CFBundleExecutable': 'taskmanager',
        'CFBundleIconFile': 'TaskManager.icns',
        'CFBundlePackageType': 'APPL',
        'CFBundleShortVersionString': '1.2.0',
        'CFBundleVersion': '1',
        'LSMinimumSystemVersion': '13.0',
        'NSHighResolutionCapable': True,
        'NSSupportsAutomaticGraphicsSwitching': True,
    }, f)
PY
codesign --force --sign - "$bundle/Contents/MacOS/missioncenter-magpie"
codesign --force --sign - "$bundle/Contents/MacOS/taskmanager"
codesign --force --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
printf '%s\n' "Built $bundle (ad-hoc signed; not notarized)."
