#!/usr/bin/env python3
"""Package a compiled core binary for independent language package installers."""
import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess

TARGETS = {
    "x86_64-unknown-linux-gnu": "linux-x64",
    "aarch64-unknown-linux-gnu": "linux-arm64",
    "x86_64-unknown-linux-musl": "linux-x64",
    "aarch64-unknown-linux-musl": "linux-arm64",
    "x86_64-apple-darwin": "darwin-x64",
    "aarch64-apple-darwin": "darwin-arm64",
    "x86_64-pc-windows-msvc": "win32-x64",
    "x86_64-pc-windows-gnu": "win32-x64",
    "aarch64-pc-windows-msvc": "win32-arm64",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--version", default="0.1.0")
    parser.add_argument("--out", type=pathlib.Path, default=pathlib.Path("target/release-packages"))
    args = parser.parse_args()
    # Host artifacts can be verified here; cross-target artifacts are verified in native CI.
    host_target = subprocess.check_output(["rustc", "-vV"], text=True).split("host: ")[1].splitlines()[0]
    if host_target == args.target:
        actual = subprocess.check_output([str(args.binary.resolve()), "--version"], text=True).strip()
        if actual != f"codefactor {args.version}":
            parser.error(f"binary version mismatch: {actual}")
    platform = TARGETS[args.target]
    filename = f"codefactor-{platform}" + (".exe" if platform.startswith("win32") else "")
    args.out.mkdir(parents=True, exist_ok=True)
    binary = args.out / filename
    shutil.copy2(args.binary, binary)
    binary.chmod(0o755)
    manifest = {
        "version": args.version,
        "platform": platform,
        "target": args.target,
        "file": filename,
        "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "size": binary.stat().st_size,
    }
    if platform.startswith("linux"):
        manifest["libc"] = "musl" if args.target.endswith("-musl") else "glibc"
    (args.out / f"manifest-{platform}.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest))


if __name__ == "__main__":
    main()
