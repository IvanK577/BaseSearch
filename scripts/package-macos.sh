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

echo '==> Installing locked frontend dependencies'
(cd web-ui && npm ci)
echo '==> Building React assets for embedding'
(cd web-ui && npm run build)
test -f web-ui/dist/index.html

echo '==> Building locked production binaries with browser and DuckDB OLAP'
cargo build --locked --release --no-default-features --features release-package \
  --bin BaseSearch --bin base-search-cli

version="$(cargo metadata --locked --no-deps --format-version 1 | \
  node -e 'let text=""; process.stdin.on("data", c => text += c); process.stdin.on("end", () => { const p=JSON.parse(text).packages.find(x => x.name === "base-search"); if (!p) process.exit(2); process.stdout.write(p.version); });')"
git_sha="$(git rev-parse --short=12 HEAD)"
package_name="BaseSearch-$version-macos-$arch"
package_dir="$output_root/$package_name"
archive_path="$output_root/$package_name.zip"
checksum_path="$archive_path.sha256"
app_dir="$package_dir/BaseSearch.app"

rm -rf -- "$package_dir"
rm -f -- "$archive_path" "$checksum_path"
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

node scripts/release-package.mjs render-readme \
  --template scripts/release/README.txt.in \
  --out "$package_dir/README.txt" \
  --platform macos \
  --arch "$arch" \
  --version "$version" \
  --git-sha "$git_sha" \
  --epoch "$source_date_epoch"

node scripts/release-package.mjs write-manifest \
  --root "$package_dir" \
  --platform macos \
  --arch "$arch" \
  --version "$version" \
  --git-sha "$git_sha" \
  --epoch "$source_date_epoch"
node scripts/release-package.mjs verify --root "$package_dir" --platform macos

echo '==> Creating normalized ZIP archive'
(cd "$output_root" && LC_ALL=C find "$package_name" -print | LC_ALL=C sort | zip -X -q "$archive_path" -@)
timestamp="$(date -u -r "$source_date_epoch" '+%Y%m%d%H%M.%S')"
touch -t "$timestamp" "$archive_path"
(cd "$output_root" && shasum -a 256 "$(basename "$archive_path")" > "$(basename "$checksum_path")")
touch -t "$timestamp" "$checksum_path"

echo "Package folder: $package_dir"
echo "Package archive: $archive_path"
cat "$checksum_path"
echo 'Signing boundary: this .app is intentionally unsigned. Sign the final .app and CLI with Developer ID, enable hardened runtime, notarize the archive, and staple the ticket before public distribution.'
