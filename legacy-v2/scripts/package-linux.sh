#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"

output_root_arg="${1:-release_packages}"
if [[ "$output_root_arg" = /* ]]; then
  output_root="$(realpath -m "$output_root_arg")"
else
  output_root="$(realpath -m "$repo_root/$output_root_arg")"
fi
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
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target/package-release/linux-$arch}"
require_signing="${BASE_SEARCH_REQUIRE_SIGNING:-0}"

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
package_name="BaseSearch-$version-linux-$arch"
package_dir="$output_root/$package_name"
archive_path="$output_root/$package_name.tar.gz"
checksum_path="$archive_path.sha256"

rm -rf -- "$package_dir"
rm -f -- "$archive_path" "$checksum_path"
mkdir -p "$package_dir/data"
install -m 0755 "$CARGO_TARGET_DIR/release/BaseSearch" "$package_dir/BaseSearch"
install -m 0755 "$CARGO_TARGET_DIR/release/base-search-cli" "$package_dir/base-search-cli"
install -m 0644 LICENSE "$package_dir/LICENSE"

cat > "$package_dir/Open Base Search.sh" <<'EOF'
#!/usr/bin/env sh
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
exec "$script_dir/BaseSearch" "$@"
EOF
chmod 0755 "$package_dir/Open Base Search.sh"

node scripts/release-package.mjs render-readme \
  --template scripts/release/README.txt.in \
  --out "$package_dir/README.txt" \
  --platform linux \
  --arch "$arch" \
  --version "$version" \
  --git-sha "$git_sha" \
  --epoch "$source_date_epoch" \
  --signing unsigned \
  --notarized false

node scripts/release-package.mjs write-manifest \
  --root "$package_dir" \
  --platform linux \
  --arch "$arch" \
  --version "$version" \
  --git-sha "$git_sha" \
  --epoch "$source_date_epoch" \
  --signing unsigned \
  --notarized false
node scripts/release-package.mjs verify \
  --root "$package_dir" \
  --platform linux \
  --require-signed "$([[ "$require_signing" == 1 ]] && echo true || echo false)"

echo '==> Creating deterministic tar.gz archive'
mkdir -p "$output_root"
tar --sort=name \
  --mtime="@$source_date_epoch" \
  --owner=0 --group=0 --numeric-owner \
  -C "$output_root" -czf "$archive_path" "$package_name"
touch -d "@$source_date_epoch" "$archive_path"
(cd "$output_root" && sha256sum "$(basename "$archive_path")" > "$(basename "$checksum_path")")
touch -d "@$source_date_epoch" "$checksum_path"

echo "Package folder: $package_dir"
echo "Package archive: $archive_path"
cat "$checksum_path"
