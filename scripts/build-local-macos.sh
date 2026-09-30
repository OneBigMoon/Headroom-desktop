#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "codexbox macOS packages must be built on macOS." >&2
  exit 1
fi

npm ci
npm run test:frontend
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --test-threads=1
cargo check --manifest-path src-tauri/Cargo.toml
npx tauri build --bundles app --config '{"bundle":{"createUpdaterArtifacts":false}}'

app_path="src-tauri/target/release/bundle/macos/codexbox.app"
dmg_dir="src-tauri/target/release/bundle/dmg"
version="$(node -p "require('./package.json').version")"

# Tauri leaves an unsigned local build with only the Mach-O linker signature.
# Seal the complete bundle so macOS can verify its resources without requiring
# an Apple Developer certificate or changing the app's local-only distribution.
codesign --force --deep --sign - --timestamp=none \
  --entitlements "src-tauri/Entitlements.plist" \
  "$app_path"
codesign --verify --deep --strict "$app_path"

case "$(uname -m)" in
  arm64) dmg_arch="aarch64" ;;
  x86_64) dmg_arch="x64" ;;
  *) dmg_arch="$(uname -m)" ;;
esac
dmg_path="$dmg_dir/codexbox_${version}_${dmg_arch}.dmg"
staging_dir="$(mktemp -d)"
cleanup() {
  if [[ -n "${staging_dir:-}" && -d "$staging_dir" ]]; then
    rm -rf -- "$staging_dir"
  fi
}
trap cleanup EXIT

mkdir -p "$dmg_dir"
ditto "$app_path" "$staging_dir/codexbox.app"
ln -s /Applications "$staging_dir/Applications"
hdiutil create \
  -volname "codexbox" \
  -srcfolder "$staging_dir" \
  -format UDZO \
  -ov \
  "$dmg_path"

echo
echo "Ad-hoc signed local artifacts:"
find src-tauri/target/release/bundle -maxdepth 3 \
  \( -name '*.app' -o -name '*.dmg' \) -print
echo
echo "This script does not use Developer ID signing, notarize, publish, or install the app."
