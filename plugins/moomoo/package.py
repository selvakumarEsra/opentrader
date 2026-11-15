"""The moomoo plugin package, for the plugin release workflow (.github/workflows/plugin-moomoo.yml).

    python package.py zip   BINARY PLATFORM OUT_DIR   # writes OUT_DIR/<zip name>, prints its path
    python package.py entry ZIP PLATFORM               # prints the catalog entry of a signed ZIP (ZIP.sig next to it)

The zip holds the program and the manifest set for PLATFORM (windows, macos, linux): its entry and its platform.
The Windows zip is moomoo-<version>.zip (the catalog's package); the others moomoo-<version>-<platform>.zip.
"""
import hashlib
import json
import os
import sys
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
RELEASES = "https://github.com/{repo}/releases/download/plugin-moomoo-v{version}/{name}"


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
    repo = os.environ.get("GITHUB_REPOSITORY", "deepentropy/opentrader")
    entry = {k: m[k] for k in ("id", "name", "version", "publisher", "description", "provides", "api")}
    entry["platforms"] = [platform]
    entry["homepage"] = m["homepage"]
    entry["url"] = RELEASES.format(repo=repo, version=m["version"], name=os.path.basename(zip_path))
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
