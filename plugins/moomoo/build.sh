#!/usr/bin/env bash
# Builds the moomoo plugin package for this computer (macOS or Linux): compiles the plugin program, zips it with the
# manifest set for this system, and signs the zip with the release key when it is there. Only a signed package gets
# a catalog entry; the catalog itself lists the Windows package (build.ps1).
#
#   ./build.sh [key]        key: default ~/.tauri/opentrader-updater.key; its password in
#                           TAURI_SIGNING_PRIVATE_KEY_PASSWORD or <key>.password
#
# Out: plugins/moomoo/dist/moomoo-<version>-<platform>.zip, and with the key .zip.sig and catalog-entry-<platform>.json.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
key="${1:-$HOME/.tauri/opentrader-updater.key}"

case "$(uname -s)" in
  Darwin) platform=macos ;;
  Linux) platform=linux ;;
  *) echo "build.sh builds on macOS or Linux; use build.ps1 on Windows" >&2; exit 1 ;;
esac

version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$here/opentrader-plugin.json")"
cargo_version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)"
if [ "$cargo_version" != "$version" ]; then
  echo "Cargo.toml says $cargo_version, opentrader-plugin.json says $version" >&2
  exit 1
fi

dist="$here/dist"
stage="$dist/stage"
rm -rf "$stage"
mkdir -p "$stage"
(cd "$here" && cargo build --release)
cp "$here/target/release/opentrader-moomoo" "$stage/opentrader-moomoo"
# The manifest of this system's package: its program and its platform.
python3 - "$here/opentrader-plugin.json" "$stage/opentrader-plugin.json" "$platform" <<'EOF'
import json, sys
m = json.load(open(sys.argv[1]))
m["runtime"]["entry"] = "opentrader-moomoo"
m["platforms"] = [sys.argv[3]]
json.dump(m, open(sys.argv[2], "w"), indent=2, ensure_ascii=False)
EOF

zip="$dist/moomoo-$version-$platform.zip"
rm -f "$zip" "$zip.sig" "$dist/catalog-entry-$platform.json"
(cd "$stage" && zip -q -X "$zip" opentrader-moomoo opentrader-plugin.json)
rm -rf "$stage"
echo "Package: $zip"

if [ ! -f "$key" ]; then
  echo "Not signed: no release key at $key. No catalog entry was written."
  exit 0
fi
if [ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ] && [ -f "$key.password" ]; then
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(tr -d '\r\n' < "$key.password")"
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD
fi
(cd "$here/../.." && npx tauri signer sign -f "$key" "$zip" > /dev/null)
if [ ! -f "$zip.sig" ]; then
  echo "The package was not signed" >&2
  exit 1
fi

python3 - "$here/opentrader-plugin.json" "$zip" "$platform" "$version" > "$dist/catalog-entry-$platform.json" <<'EOF'
import hashlib, json, os, sys
manifest, zip_path, platform, version = sys.argv[1:]
m = json.load(open(manifest))
entry = {k: m[k] for k in ("id", "name", "version", "publisher", "description", "provides", "api")}
entry["platforms"] = [platform]
entry["homepage"] = m["homepage"]
entry["url"] = f"https://github.com/selvakumarEsra/opentrader/releases/download/plugin-moomoo-v{version}/{os.path.basename(zip_path)}"
entry["sha256"] = hashlib.sha256(open(zip_path, "rb").read()).hexdigest()
entry["signature"] = open(zip_path + ".sig").read().strip()
print(json.dumps(entry, indent=2, ensure_ascii=False))
EOF
echo "Catalog entry: $dist/catalog-entry-$platform.json"
