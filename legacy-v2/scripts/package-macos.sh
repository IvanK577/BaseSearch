#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"

output_root_arg="${1:-release_packages}"
if [[ "$output_root_arg" = /* ]]; then
  output_root="$output_root_arg"
else
  output_root="$repo_root/$output_root_arg"
fi
mkdir -p "$output_root"
output_root="$(cd "$output_root" && pwd -P)"
case "$output_root/" in
  "$repo_root"/*) ;;
  *) echo "Release paths must stay inside the repository: $output_root" >&2; exit 2 ;;
esac

case "$(uname -m)" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) arch="$(uname -m)" ;;
esac

if [[ "${SOURCE_DATE_EPOCH:-}" =~ ^[0-9]+$ ]]; then
  source_date_epoch="$SOURCE_DATE_EPOCH"
else
  source_date_epoch="$(git log -1 --format=%ct)"
fi
export SOURCE_DATE_EPOCH="$source_date_epoch"
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target/package-release/macos-$arch}"

require_signing="${BASE_SEARCH_REQUIRE_SIGNING:-0}"
signing_identity="${BASE_SEARCH_MACOS_SIGN_IDENTITY:-}"
apple_id="${BASE_SEARCH_APPLE_ID:-}"
apple_team_id="${BASE_SEARCH_APPLE_TEAM_ID:-}"
apple_app_password="${BASE_SEARCH_APP_PASSWORD:-}"
notary_values=0
[[ -n "$apple_id" ]] && notary_values=$((notary_values + 1))
[[ -n "$apple_team_id" ]] && notary_values=$((notary_values + 1))
[[ -n "$apple_app_password" ]] && notary_values=$((notary_values + 1))
if [[ "$notary_values" -ne 0 && "$notary_values" -ne 3 ]]; then
  echo 'macOS notarization is partially configured. Set BASE_SEARCH_APPLE_ID, BASE_SEARCH_APPLE_TEAM_ID, and BASE_SEARCH_APP_PASSWORD together.' >&2
  exit 2
fi
if [[ "$notary_values" -eq 3 && -z "$signing_identity" ]]; then
  echo 'BASE_SEARCH_MACOS_SIGN_IDENTITY is required when notarization credentials are configured.' >&2
  exit 2
fi
if [[ "$require_signing" == 1 && -z "$signing_identity" ]]; then
  echo 'Stable tag packaging requires BASE_SEARCH_MACOS_SIGN_IDENTITY and an imported Developer ID Application certificate.' >&2
  exit 2
fi
if [[ "$require_signing" == 1 && "$notary_values" -ne 3 ]]; then
  echo 'Stable tag packaging requires BASE_SEARCH_APPLE_ID, BASE_SEARCH_APPLE_TEAM_ID, and BASE_SEARCH_APP_PASSWORD for notarization.' >&2
  exit 2
fi
signing_state=unsigned
notarized=false

echo '==> Installing locked frontend dependencies'
(cd web-ui && npm ci)
echo '==> Building React assets for embedding'
(cd web-ui && npm run build)
test -f web-ui/dist/index.html

echo '==> Building locked production binaries with the browser workspace'
cargo build --locked --release --no-default-features --features release-package \
  --bin BaseSearch --bin base-search-cli

version="$(cargo metadata --locked --no-deps --format-version 1 | \
  node -e 'let text=""; process.stdin.on("data", c => text += c); process.stdin.on("end", () => { const p=JSON.parse(text).packages.find(x => x.name === "base-search"); if (!p) process.exit(2); process.stdout.write(p.version); });')"
git_sha="$(git rev-parse --short=12 HEAD)"
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
  git_sha="${git_sha}-dirty"
fi
package_name="BaseSearch-$version-macos-$arch"
package_dir="$output_root/$package_name"
archive_path="$output_root/$package_name.zip"
checksum_path="$archive_path.sha256"
app_dir="$package_dir/BaseSearch.app"
notary_archive="$output_root/.$package_name.notarization.zip"

cleanup() {
  rm -f -- "$notary_archive"
}
trap cleanup EXIT

rm -rf -- "$package_dir"
rm -f -- "$archive_path" "$checksum_path" "$notary_archive"
mkdir -p "$app_dir/Contents/MacOS" "$package_dir/data"
install -m 0755 "$CARGO_TARGET_DIR/release/BaseSearch" "$app_dir/Contents/MacOS/BaseSearch"
install -m 0755 "$CARGO_TARGET_DIR/release/base-search-cli" "$package_dir/base-search-cli"
install -m 0644 LICENSE "$package_dir/LICENSE"

cat > "$app_dir/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key>
  <string>Base Search</string>
  <key>CFBundleExecutable</key>
  <string>BaseSearch</string>
  <key>CFBundleIdentifier</key>
  <string>com.basesearch.workspace</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Base Search</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$version</string>
  <key>CFBundleVersion</key>
  <string>$version</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
EOF

if [[ -n "$signing_identity" ]]; then
  echo '==> Developer ID signing final macOS binaries'
  codesign --force --timestamp --options runtime --sign "$signing_identity" \
    "$package_dir/base-search-cli"
  codesign --force --timestamp --options runtime --sign "$signing_identity" \
    "$app_dir"
  codesign --verify --strict --verbose=2 "$package_dir/base-search-cli"
  codesign --verify --deep --strict --verbose=2 "$app_dir"
  signing_state=signed
else
  echo '==> codesign skipped for this local developer package'
fi

if [[ "$notary_values" -eq 3 ]]; then
  echo '==> Submitting signed macOS package for notarization'
  ditto -c -k --keepParent "$package_dir" "$notary_archive"
  xcrun notarytool submit "$notary_archive" \
    --apple-id "$apple_id" \
    --team-id "$apple_team_id" \
    --password "$apple_app_password" \
    --wait
  xcrun stapler staple "$app_dir"
  xcrun stapler validate "$app_dir"
  codesign --verify --deep --strict --verbose=2 "$app_dir"
  notarized=true
  rm -f -- "$notary_archive"
fi

node scripts/release-package.mjs render-readme \
  --template scripts/release/README.txt.in \
  --out "$package_dir/README.txt" \
  --platform macos \
  --arch "$arch" \
  --version "$version" \
  --git-sha "$git_sha" \
  --epoch "$source_date_epoch" \
  --signing "$signing_state" \
  --notarized "$notarized"

node scripts/release-package.mjs write-manifest \
  --root "$package_dir" \
  --platform macos \
  --arch "$arch" \
  --version "$version" \
  --git-sha "$git_sha" \
  --epoch "$source_date_epoch" \
  --signing "$signing_state" \
  --notarized "$notarized"
if [[ "$signing_state" == signed ]]; then
  codesign --verify --strict --verbose=2 "$package_dir/base-search-cli"
  codesign --verify --deep --strict --verbose=2 "$app_dir"
fi
if [[ "$notarized" == true ]]; then
  xcrun stapler validate "$app_dir"
fi
node scripts/release-package.mjs verify \
  --root "$package_dir" \
  --platform macos \
  --require-signed "$([[ "$require_signing" == 1 ]] && echo true || echo false)"

if [[ "$notarized" == true ]]; then
  echo '==> Creating metadata-preserving notarized ZIP archive'
  ditto -c -k --keepParent "$package_dir" "$archive_path"
else
  echo '==> Creating normalized ZIP archive'
  (cd "$output_root" && LC_ALL=C find "$package_name" -print | LC_ALL=C sort | zip -X -q "$archive_path" -@)
fi
timestamp="$(date -u -r "$source_date_epoch" '+%Y%m%d%H%M.%S')"
touch -t "$timestamp" "$archive_path"
(cd "$output_root" && shasum -a 256 "$(basename "$archive_path")" > "$(basename "$checksum_path")")
touch -t "$timestamp" "$checksum_path"

echo "Package folder: $package_dir"
echo "Package archive: $archive_path"
cat "$checksum_path"
if [[ "$signing_state" == unsigned ]]; then
  echo 'Signing boundary: this local .app is unsigned. Stable tag packaging requires Developer ID signing, notarization, and stapling.'
fi
