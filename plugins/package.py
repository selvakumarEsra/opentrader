"""The package of a data plugin (moomoo, zerodha, dhan), for the plugin release workflows
(.github/workflows/plugin-moomoo.yml, plugin-india.yml).

    python plugins/package.py zip   PLUGIN BINARY PLATFORM OUT_DIR   # writes the program and the zip, prints the zip
    python plugins/package.py entry PLUGIN ZIP PLATFORM               # prints the catalog entry of a signed ZIP

PLUGIN is the plugin's folder name under plugins/ (its id). BINARY is the plugin's one program, written as
opentrader-<id>-<version>-<system> (windows-x86_64.exe, macos-universal, linux-x86_64) and zipped with the manifest
set for PLATFORM (windows, macos, linux). The Windows zip is <id>-<version>.zip; the others <id>-<version>-<platform>.zip.

Releases are published to selvakumarEsra/opentrader, never upstream: catalog entries point there.
"""
import hashlib
import json
import os
import shutil
import sys
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
RELEASE_REPO = "selvakumarEsra/opentrader"
RELEASES = "https://github.com/{repo}/releases/download/{tag}/{name}"


def release_tag(plugin, version):
    """The release the zip is uploaded to: the app's (RELEASE_TAG, set by the workflow) or the plugin's own tag."""
    return os.environ.get("RELEASE_TAG") or f"plugin-{plugin}-v{version}"
SYSTEMS = {"windows": "windows-x86_64.exe", "macos": "macos-universal", "linux": "linux-x86_64"}


def manifest(plugin):
    with open(os.path.join(HERE, plugin, "opentrader-plugin.json"), encoding="utf-8") as f:
        return json.load(f)


def zip_name(plugin, version, platform):
    return f"{plugin}-{version}.zip" if platform == "windows" else f"{plugin}-{version}-{platform}.zip"


def write(z, name, data, mode):
    info = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
    info.external_attr = mode << 16
    info.compress_type = zipfile.ZIP_DEFLATED
    z.writestr(info, data)


def make_zip(plugin, binary, platform, out_dir):
    m = manifest(plugin)
    entry = f"opentrader-{plugin}.exe" if platform == "windows" else f"opentrader-{plugin}"
    m["runtime"]["entry"] = entry
    m["platforms"] = [platform]
    os.makedirs(out_dir, exist_ok=True)
    single = os.path.join(out_dir, f"opentrader-{plugin}-{m['version']}-{SYSTEMS[platform]}")
    shutil.copyfile(binary, single)
    os.chmod(single, 0o755)
    path = os.path.join(out_dir, zip_name(plugin, m["version"], platform))
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        with open(binary, "rb") as f:
            write(z, entry, f.read(), 0o755)  # executable once unpacked on macOS and Linux
        write(z, "opentrader-plugin.json", json.dumps(m, indent=2, ensure_ascii=False), 0o644)
    print(path)


def catalog_entry(plugin, zip_path, platform):
    m = manifest(plugin)
    with open(zip_path, "rb") as f:
        sha = hashlib.sha256(f.read()).hexdigest()
    with open(zip_path + ".sig", encoding="utf-8") as f:
        signature = f.read().strip()
    entry = {k: m[k] for k in ("id", "name", "version", "publisher", "description", "provides", "api")}
    entry["platforms"] = [platform]
    entry["homepage"] = m["homepage"]
    entry["url"] = RELEASES.format(repo=RELEASE_REPO, tag=release_tag(plugin, m["version"]), name=os.path.basename(zip_path))
    entry["sha256"] = sha
    entry["signature"] = signature
    print(json.dumps(entry, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    if len(sys.argv) == 6 and sys.argv[1] == "zip":
        make_zip(*sys.argv[2:])
    elif len(sys.argv) == 5 and sys.argv[1] == "entry":
        catalog_entry(*sys.argv[2:])
    else:
        sys.exit(__doc__)
