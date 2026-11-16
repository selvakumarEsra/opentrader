"""The moomoo plugin package, for the plugin release workflow (.github/workflows/plugin-moomoo.yml).

    python package.py zip   BINARY PLATFORM OUT_DIR   # writes the program and the zip to OUT_DIR, prints the zip's path
    python package.py entry ZIP PLATFORM               # prints the catalog entry of a signed ZIP (ZIP.sig next to it)

BINARY is the whole plugin in one program (the app's plugin, `backfill`, `record`, the embedded store). It is
written as opentrader-moomoo-<version>-<system> (windows-x86_64.exe, macos-universal, linux-x86_64), and zipped
with the manifest set for PLATFORM (windows, macos, linux): its entry and its platform. The Windows zip is
moomoo-<version>.zip (the catalog's package); the others moomoo-<version>-<platform>.zip.

Releases are published to selvakumarEsra/opentrader, never upstream: catalog entries point there.
"""
import hashlib
import json
import os
import shutil
import sys
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
# The repository plugin releases are published to.
RELEASE_REPO = "selvakumarEsra/opentrader"
RELEASES = "https://github.com/{repo}/releases/download/{tag}/{name}"


def release_tag(version):
    """The release the zip is uploaded to: the app's (RELEASE_TAG, set by the workflow) or the plugin's own tag."""
    return os.environ.get("RELEASE_TAG") or f"plugin-moomoo-v{version}"
SYSTEMS = {"windows": "windows-x86_64.exe", "macos": "macos-universal", "linux": "linux-x86_64"}


def manifest():
    with open(os.path.join(HERE, "opentrader-plugin.json"), encoding="utf-8") as f:
        return json.load(f)


def zip_name(version, platform):
    return f"moomoo-{version}.zip" if platform == "windows" else f"moomoo-{version}-{platform}.zip"


def make_zip(binary, platform, out_dir):
    m = manifest()
    entry = "opentrader-moomoo.exe" if platform == "windows" else "opentrader-moomoo"
    m["runtime"]["entry"] = entry
    m["platforms"] = [platform]
    os.makedirs(out_dir, exist_ok=True)
    single = os.path.join(out_dir, f"opentrader-moomoo-{m['version']}-{SYSTEMS[platform]}")
    shutil.copyfile(binary, single)
    os.chmod(single, 0o755)
    path = os.path.join(out_dir, zip_name(m["version"], platform))
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        info = zipfile.ZipInfo(entry, date_time=(2020, 1, 1, 0, 0, 0))
        info.external_attr = 0o755 << 16  # executable once unpacked on macOS and Linux
        info.compress_type = zipfile.ZIP_DEFLATED
        with open(binary, "rb") as f:
            z.writestr(info, f.read())
        info = zipfile.ZipInfo("opentrader-plugin.json", date_time=(2020, 1, 1, 0, 0, 0))
        info.external_attr = 0o644 << 16
        info.compress_type = zipfile.ZIP_DEFLATED
        z.writestr(info, json.dumps(m, indent=2, ensure_ascii=False))
    print(path)


def catalog_entry(zip_path, platform):
    m = manifest()
    with open(zip_path, "rb") as f:
        sha = hashlib.sha256(f.read()).hexdigest()
    with open(zip_path + ".sig", encoding="utf-8") as f:
        signature = f.read().strip()
    entry = {k: m[k] for k in ("id", "name", "version", "publisher", "description", "provides", "api")}
    entry["platforms"] = [platform]
    entry["homepage"] = m["homepage"]
    entry["url"] = RELEASES.format(repo=RELEASE_REPO, tag=release_tag(m["version"]), name=os.path.basename(zip_path))
    entry["sha256"] = sha
    entry["signature"] = signature
    print(json.dumps(entry, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "zip":
        make_zip(*sys.argv[2:])
    elif len(sys.argv) == 4 and sys.argv[1] == "entry":
        catalog_entry(*sys.argv[2:])
    else:
        sys.exit(__doc__)
