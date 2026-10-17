#!/usr/bin/env python3
"""Build a portable Agent2.3 Launcher + versioned Core/Desktop package.

All targets remain per-user, require no drivers or system-wide installation.
The archive does not register OS schemes or Chrome Native Hosts automatically.
"""
import json
import pathlib
import plistlib
import shutil
import subprocess
import sys
import tomllib
import zipfile

root = pathlib.Path(__file__).resolve().parents[1]
subprocess.run(["cargo", "build", "--workspace", "--release", "--locked"], cwd=root, check=True)
target = subprocess.check_output(["rustc", "-vV"], cwd=root, text=True).split("host: ")[1].splitlines()[0]
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
stage = root / "dist" / ("cloudfile-agent23-" + target)
if stage.exists():
    shutil.rmtree(stage)
stage.mkdir(parents=True)
app = None
base = stage
if sys.platform == "darwin":
    app = stage / "CloudFile Agent 2.3 Preview.app"
    base = app / "Contents" / "MacOS"
    base.mkdir(parents=True)
    with (app / "Contents" / "Info.plist").open("wb") as stream:
        plistlib.dump({
            "CFBundleName": "CloudFile Agent 2.3 Preview",
            "CFBundleDisplayName": "CloudFile Agent 2.3 Preview",
            "CFBundleIdentifier": "com.cloudfile.agent23.dev",
            "CFBundleExecutable": "cloudfile-launcher",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": version.split("-")[0],
            "CFBundleVersion": version.split("-")[0],
            "CloudFileAgentBuildVersion": version,
            "NSHighResolutionCapable": True,
        }, stream)

exe = ".exe" if sys.platform == "win32" else ""
shutil.copy2(root / "target" / "release" / ("cloudfile-launcher" + exe),
             base / ("cloudfile-launcher" + exe))
version_bin = base if app else base / "bin" / version
version_bin.mkdir(parents=True, exist_ok=True)
for filename in ("cloudfile-native-host", "cloudfile-desktop"):
    shutil.copy2(root / "target" / "release" / (filename + exe), version_bin / (filename + exe))
(base / "current.json").write_text(json.dumps({"version": version, "previous": None}) + "\n")

if app:
    # Development build only: proper production signing/notarization is a
    # separate step. Installed macOS upgrades replace the *whole* app Bundle.
    subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(app)], check=True)

shutil.copy2(root / "README.md", stage / "README.md")
archive = stage.with_suffix(".zip")
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zipper:
    for path in sorted(stage.rglob("*")):
        if path.is_file():
            zipper.write(path, path.relative_to(stage))
print(f"{archive}: {archive.stat().st_size / 1024 / 1024:.1f} MiB")
