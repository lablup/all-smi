#!/usr/bin/env python3

import json
from pathlib import Path


def should_strip(rel_path: str) -> bool:
    path = Path(rel_path)
    return (
        rel_path.endswith(".orig")
        or rel_path.startswith("test-data/")
        or any(part.startswith(".") for part in path.parts)
    )


def main() -> int:
    vendor_root = Path("vendor")
    if not vendor_root.is_dir():
        raise SystemExit("vendor directory not found")

    checksum_files = list(vendor_root.rglob(".cargo-checksum.json"))
    for checksum_file in checksum_files:
        crate_dir = checksum_file.parent
        data = json.loads(checksum_file.read_text())
        files = data.get("files", {})

        removed = []
        for rel_path in list(files.keys()):
            target = crate_dir / rel_path
            if should_strip(rel_path) or not target.exists():
                removed.append(rel_path)
                files.pop(rel_path, None)
                if target.exists():
                    target.unlink()

        if removed:
            checksum_file.write_text(
                json.dumps(data, separators=(",", ":"), sort_keys=True) + "\n"
            )
            print(f"sanitized {checksum_file} ({len(removed)} hidden/orig entries)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
