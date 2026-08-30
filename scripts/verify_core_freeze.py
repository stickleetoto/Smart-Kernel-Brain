from __future__ import annotations

import hashlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXPECTED = {
    "src/engine.rs": "8ef8bbcf36fdb7b6688bf3ab39670cc8aecb24fe9828410fb14a2f56814e3df7",
    "src/hash.rs": "37c03c3505daf87827e65b1a49b0b0d837f6bdda0c608109b268cd51dad73e27",
    "src/index.rs": "88e3f4c774d45149428834c5573601656d947d1265084f786af80e1fc83c9052",
    "src/lib.rs": "1f229eec8b4eef7a408451e3bcdeb3720b6020b1a22129b97a0424f0313aeaf0",
    "src/paths.rs": "a1080e8e23a90536dc834d41f324be1d725b6b8c2f35b29a049f479bd80ded7c",
    "src/resident.rs": "9e2abbf1f504f8bef94db57a84007b9d334f830d37f708d7e0c0d49d5ffc44a4",
    "src/state.rs": "354b51c3efc9b671ccce9a4c242b64ff1753d9b84a1501a281bd63e55c9fd12b",
}

failed = False
for rel, expected in EXPECTED.items():
    path = ROOT / rel
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    status = "OK" if actual == expected else "CHANGED"
    print(f"{status:7} {rel} {actual}")
    failed |= actual != expected

if failed:
    raise SystemExit("core freeze verification failed")
print("core freeze verification passed")
