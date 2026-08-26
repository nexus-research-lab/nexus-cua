#!/bin/sh
set -eu

fixture_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(CDPATH= cd -- "$fixture_root/../../.." && pwd)
configuration=${1:-debug}
output_root="$repository_root/target/native-fixtures/macos"
app_root="$output_root/Nexus CUA Native Fixture.app"

swift build --package-path "$fixture_root" --configuration "$configuration"
binary_path=$(swift build --package-path "$fixture_root" --configuration "$configuration" --show-bin-path)

rm -rf "$app_root"
mkdir -p "$app_root/Contents/MacOS"
cp "$fixture_root/Info.plist" "$app_root/Contents/Info.plist"
cp "$binary_path/nexus-cua-native-fixture" "$app_root/Contents/MacOS/nexus-cua-native-fixture"
codesign --force --sign - --timestamp=none "$app_root"
printf '%s\n' "$app_root"
