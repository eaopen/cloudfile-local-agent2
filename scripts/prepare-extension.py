#!/usr/bin/env python3
"""Create an unpacked dev extension without modifying the shared extension repo.

The shared Chrome extension lives in its own repository, so this build-time
helper must be told where it is. Resolution order:

1. ``--extension-src`` command line argument
2. ``CLOUDFILE_EXTENSION_SRC`` environment variable
3. ``../cloudfile-chrome-extension`` or ``../../cloudfile-chrome-extension``

The copy stays under ``dist/`` and is never committed; the shared extension
repository is never written to.
"""
import argparse
import json
import os
import pathlib
import shutil
import re
import sys

DEV_HOST = 'com.cloudfile.agent22.dev'
# macOS development builds use an isolated Rust Host; Windows must retain
# the fixed controller Host that cloudfile-core::hostreg::HOST registers.
MACOS_UPSTREAM_HOST = 'com.cloudfile.agent2'
WINDOWS_HOST = 'com.cloudfile.current_agent'

root = pathlib.Path(__file__).resolve().parents[1]
default_target = root / 'dist' / 'chrome-extension-agent22'


def locate_extension(explicit):
    if explicit:
        return pathlib.Path(explicit).expanduser(), '--extension-src'
    env = os.environ.get('CLOUDFILE_EXTENSION_SRC')
    if env:
        return pathlib.Path(env).expanduser(), 'CLOUDFILE_EXTENSION_SRC'
    for candidate in (root.parent / 'cloudfile-chrome-extension',
                      root.parent.parent / 'cloudfile-chrome-extension'):
        if candidate.is_dir():
            return candidate, 'sibling directory'
    return None, None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--extension-src',
                        help='path to the shared cloudfile-chrome-extension checkout')
    parser.add_argument('--out',
                        help=f'unpacked output directory (default: {default_target})')
    args = parser.parse_args()

    source, origin = locate_extension(args.extension_src)
    if source is None:
        sys.exit(
            'error: cannot locate the shared Chrome extension source.\n'
            'Pass --extension-src <path> or set CLOUDFILE_EXTENSION_SRC.\n'
            f'Looked for: {root.parent / "cloudfile-chrome-extension"}, '
            f'{root.parent.parent / "cloudfile-chrome-extension"}'
        )
    if not source.is_dir():
        sys.exit(f'error: extension source does not exist: {source}')
    if not (source / 'manifest.json').is_file():
        sys.exit(f'error: {source} is not a Chrome extension (no manifest.json)')

    # Validate the shared extension contract before deleting/replacing any
    # previously prepared development copy.
    background = source / 'background.js'
    if not background.is_file():
        sys.exit(f"error: missing {background}")
    s = background.read_text()
    # Rewrite only macOS Agent2 routing. Windows always uses current_agent.
    upstream = re.compile(
        r"(?m)^const\s+AGENT2_HOST\s*=\s*['\"]"
        + re.escape(MACOS_UPSTREAM_HOST)
        + r"['\"];?"
    )
    s, count = upstream.subn(f"const AGENT2_HOST = '{DEV_HOST}';", s)
    if count != 1 or not re.search(
        r"(?m)^const\s+CURRENT_HOST\s*=\s*['\"]"
        + re.escape(WINDOWS_HOST)
        + r"['\"];?", s
    ):
        sys.exit(
            "error: upstream native messaging Host routing contract changed; "
            "review background.js before packaging a development extension"
        )

    target = pathlib.Path(args.out).expanduser() if args.out else default_target
    if target.exists():
        shutil.rmtree(target)
    shutil.copytree(source, target,
                    ignore=shutil.ignore_patterns('.git', 'node_modules', '.DS_Store'))
    (target / 'background.js').write_text(s)

    p = target / 'manifest.json'
    m = json.loads(p.read_text())
    m['name'] = 'CloudFile Agent 2.2 Dev'
    p.write_text(json.dumps(m, ensure_ascii=False, indent=2))

    print(f'{target}\n  source: {source} ({origin})\n  macOS dev Host: {DEV_HOST}\n  Windows Host: {WINDOWS_HOST}')


if __name__ == '__main__':
    main()
