#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo 'Usage: smoke-package.sh <linux|macos> <package-directory>' >&2
  exit 2
fi

platform="$1"
package_dir="$(cd "$2" && pwd -P)"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

case "$platform" in
  linux)
    launcher="$package_dir/BaseSearch"
    cli="$package_dir/base-search-cli"
    ;;
  macos)
    launcher="$package_dir/BaseSearch.app/Contents/MacOS/BaseSearch"
    cli="$package_dir/base-search-cli"
    ;;
  *)
    echo "Unsupported platform: $platform" >&2
    exit 2
    ;;
esac

node "$repo_root/scripts/release-package.mjs" verify --root "$package_dir" --platform "$platform"

port="$(node -e 'const net=require("node:net"); const s=net.createServer(); s.listen(0,"127.0.0.1",()=>{process.stdout.write(String(s.address().port)); s.close();});')"
temp_root="$(mktemp -d "${TMPDIR:-/tmp}/base-search-package-smoke.XXXXXX")"
database_path="$temp_root/smoke.db"
stdout_path="$temp_root/server.stdout.log"
stderr_path="$temp_root/server.stderr.log"
server_pid=''

cleanup() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf -- "$temp_root"
}
trap cleanup EXIT

"$launcher" --browser --db "$database_path" --host 127.0.0.1 --port "$port" --no-open \
  >"$stdout_path" 2>"$stderr_path" &
server_pid=$!

ready=0
for _ in $(seq 1 600); do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    echo 'Packaged server exited before readiness.' >&2
    cat "$stdout_path" >&2 || true
    cat "$stderr_path" >&2 || true
    exit 1
  fi
  if curl --fail --silent --max-time 2 "http://127.0.0.1:$port/api/v2/health" | \
      node -e 'let t=""; process.stdin.on("data",c=>t+=c); process.stdin.on("end",()=>process.exit(JSON.parse(t).status==="ok"?0:1));' 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.2
done
if [[ "$ready" -ne 1 ]]; then
  echo 'Packaged server did not become ready.' >&2
  exit 1
fi

curl --fail --silent --max-time 10 "http://127.0.0.1:$port/" | grep -q '<div id="root"'
curl --fail --silent --max-time 30 "http://127.0.0.1:$port/api/v2/engines" | \
  node -e 'let t=""; process.stdin.on("data",c=>t+=c); process.stdin.on("end",()=>{const v=JSON.parse(t); if(v.duckdb_available!==true){console.error("DuckDB capability is absent");process.exit(1);}});'

kill "$server_pid"
wait "$server_pid" || true
server_pid=''

"$cli" stats "$database_path"
"$cli" olap-build "$database_path"
echo "Package smoke passed: $package_dir"
