"""Optional independent Windows snapshot checker; Python stdlib only.

Reads only explicitly selected fixture/inventory beneath this worktree's
engineering run directory. No journal calls, elevation, writes, or cleanup.
Rust/PowerShell prototype execution does not depend on this checker.
"""
import argparse
import ctypes
import json
import os
import struct
from pathlib import Path

MAX_BYTES = 2 * 1024 * 1024
MAX_ENTRIES = 8192


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def raw(s):
    b = s.encode("utf-16-le", "surrogatepass")
    return tuple(struct.unpack("<" + "H" * (len(b) // 2), b))


def text(units):
    return struct.pack("<" + "H" * len(units), *units).decode("utf-16-le", "surrogatepass")


def reject_reparse_chain(selected):
    # Inspect from drive root downward: never traverse a later component before
    # rejecting an alternate target in an earlier engineering-path component.
    for component in [*reversed(selected.parents), selected]:
        require(not (component.lstat().st_file_attributes & 0x400),
                "reparse component in engineering path")


def selected_paths(fixture, inventory):
    literal_script = Path(os.path.abspath(__file__))
    literal_run = literal_script.parents[2] / ".scratch/windows-ntfs-stage-a/run"
    reject_reparse_chain(literal_run)
    run = literal_run.resolve(strict=True)
    fixture = Path(os.path.abspath(fixture))
    inventory = Path(os.path.abspath(inventory))
    require(fixture.is_relative_to(run) and inventory.is_relative_to(run), "selected paths outside engineering run")
    require(fixture.name.startswith("fixture-"), "not a selected engineering fixture")
    require(inventory.name == "inventory.lcusn", "not selected inventory.lcusn")
    require(not inventory.is_relative_to(fixture), "inventory must be outside fixture scope")
    # Check original components before is_dir/is_file or resolve follows them.
    for selected in [fixture, inventory]:
        reject_reparse_chain(selected)
    require(fixture.is_dir(), "selected fixture is not a directory")
    require(inventory.is_file(), "selected inventory is not a file")
    return fixture.resolve(strict=True), inventory.resolve(strict=True)


def decode(inventory_file, fixture):
    with inventory_file.open("rb") as handle:
        b = handle.read(MAX_BYTES + 1)
    require(72 <= len(b) <= MAX_BYTES, "snapshot byte budget")
    require(b[:8] == b"LCUSNSN\0", "snapshot magic")
    require(struct.unpack_from("<II", b, 8) == (1, len(b)), "version or declared format length")
    checksum = 0xcbf29ce484222325
    for unit in b[:-8]:
        checksum = ((checksum ^ unit) * 0x100000001b3) & ((1 << 64) - 1)
    require(checksum == int.from_bytes(b[-8:], "little"), "snapshot checksum")
    volume, journal, cursor = struct.unpack_from("<QQq", b, 16)
    root_id = int.from_bytes(b[40:56], "little")
    scope_len, count = struct.unpack_from("<II", b, 56)
    require(cursor >= 0 and root_id != 0, "cursor or root identity")
    require(0 < scope_len <= 32768 and count <= MAX_ENTRIES, "scope/inventory budget")
    end = len(b) - 8
    require(64 + scope_len * 2 <= end, "truncated scope")
    scope = struct.unpack_from("<" + "H" * scope_len, b, 64)
    expected = str(fixture)
    if not expected.startswith("\\\\?\\"):
        require(len(expected) > 2 and expected[1:3] == ":\\", "fixture must use drive-backed path")
        expected = "\\\\?\\" + expected
    require(scope == raw(expected), "scope differs from explicit fixture; refuse traversal")
    require(0 not in scope, "scope NUL")
    entries, objects, directories, namespace = [], {}, {}, set()
    offset = 64 + scope_len * 2
    for _ in range(count):
        require(offset + 40 <= end, "truncated record header")
        typ, n, attr = struct.unpack_from("<HHI", b, offset)
        require(typ == 1 and 0 < n <= 255, "record type/name budget")
        parent = int.from_bytes(b[offset + 8:offset + 24], "little")
        obj = int.from_bytes(b[offset + 24:offset + 40], "little")
        require(offset + 40 + n * 2 <= end, "truncated name")
        name = struct.unpack_from("<" + "H" * n, b, offset + 40)
        offset += 40 + n * 2
        require(name not in [(46,), (46, 46)] and not any(c in [0, 47, 58, 92] for c in name), "invalid namespace component")
        require(parent and obj and obj != root_id and obj != parent, "invalid entry identity")
        require((parent, name) not in namespace, "duplicate parent/name")
        namespace.add((parent, name))
        require(obj not in objects or objects[obj] == attr, "conflicting object attributes")
        objects[obj] = attr
        if attr & 16:
            require(obj not in directories, "multiple names for directory")
            directories[obj] = parent
        entries.append((parent, obj, name, attr))
    require(offset == end, "trailing snapshot data")
    for parent, obj, name, attr in entries:
        require(parent == root_id or objects.get(parent, 0) & (16 | 0x400) == 16, "missing/non-directory/reparse parent")
    paths = {root_id: ()}
    directory_names = {obj: name for parent, obj, name, attr in entries if attr & 16}
    remaining = dict(directories)
    while remaining:
        progressed = False
        for obj, parent in list(remaining.items()):
            if parent in paths:
                paths[obj] = paths[parent] + ((92,) if paths[parent] else ()) + directory_names[obj]
                del remaining[obj]
                progressed = True
        require(progressed, "directory cycle or unrooted graph")
    inventory = {paths[parent] + ((92,) if paths[parent] else ()) + name: (obj, attr) for parent, obj, name, attr in entries}
    require(len(inventory) == count, "duplicate resolved paths")
    return text(scope), inventory, volume, journal, cursor, root_id, len(b)


def scan(root):
    actual = {}
    pending = [(root, ())]
    while pending:
        directory, relative = pending.pop()
        with os.scandir(directory) as iterator:
            for entry in iterator:
                rel = relative + ((92,) if relative else ()) + raw(entry.name)
                info = entry.stat(follow_symlinks=False)
                require(len(actual) < MAX_ENTRIES, "actual fixture exceeds entry budget")
                require(len(rel) <= 32768, "actual path exceeds raw-name budget")
                actual[rel] = (entry.path, info)
                if info.st_file_attributes & 16 and not info.st_file_attributes & 0x400:
                    pending.append((entry.path, rel))
    return actual


k = ctypes.WinDLL("kernel32", use_last_error=True)
k.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32,
                         ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
k.CreateFileW.restype = ctypes.c_void_p
k.CloseHandle.argtypes = [ctypes.c_void_p]


class INFO(ctypes.Structure):
    _fields_ = [("attributes", ctypes.c_uint32), ("times", ctypes.c_uint32 * 6),
                ("volume", ctypes.c_uint32), ("size_hi", ctypes.c_uint32),
                ("size_lo", ctypes.c_uint32), ("links", ctypes.c_uint32),
                ("index_hi", ctypes.c_uint32), ("index_lo", ctypes.c_uint32)]


k.GetFileInformationByHandle.argtypes = [ctypes.c_void_p, ctypes.POINTER(INFO)]


def ident(path, want_links=False):
    # Same legacy 32-bit serial / 64-bit file reference as the NTFS probe.
    # OPEN_REPARSE_POINT queries the selected entry rather than its target.
    handle = k.CreateFileW(path, 0, 7, None, 3, 0x02000000 | 0x00200000, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        info = INFO()
        if not k.GetFileInformationByHandle(handle, ctypes.byref(info)):
            raise ctypes.WinError(ctypes.get_last_error())
        return info.links if want_links else (info.volume, (info.index_hi << 32) | info.index_lo)
    finally:
        if not k.CloseHandle(handle):
            raise ctypes.WinError(ctypes.get_last_error())


def run(args):
    fixture, inventory_file = selected_paths(args.fixture, args.inventory)
    root, inventory, volume, journal, cursor, root_id, file_bytes = decode(inventory_file, fixture)
    # Scope was validated before native root access or directory traversal.
    require(ident(root) == (volume, root_id), "root native identity differs from persisted scope")
    actual = scan(root)
    require(set(inventory) == set(actual), "full raw UTF16 path set mismatch")
    for rel, (path, info) in actual.items():
        require(ident(path) == (volume, inventory[rel][0]), "native entry identity mismatch")
        require(info.st_file_attributes == inventory[rel][1], "native attributes mismatch")
    require(set(scan(root)) == set(actual), "fixture changed during crosscheck")
    result = dict(full_raw_utf16_set_equal=True, snapshot_entries=len(inventory), independent_traversal_entries=len(actual),
                  all_native_legacy_volume_object_attributes_equal=True, scope_exact_checked_before_traversal=True,
                  root_native_identity_guard=True, raw_unpaired_surrogate_retained=any(0xd800 in p for p in actual),
                  checkpoint_checksum_valid=True, snapshot_file_bytes=file_bytes, journal_id=journal,
                  usn_cursor=cursor, million_file_monitoring_proven=False)
    if args.verify_native_fixture:
        for old in ["offline-delete.txt", "offline-rename-before.txt", "offline-dir-before", "offline-link-remove.txt"]:
            require(raw(old) not in actual, "owned fixture old name survived")
        for new in ["offline-added.txt", "offline-rename-after.txt", "offline-dir-after"]:
            require(raw(new) in actual, "owned fixture new name missing")
        source, added = raw("offline-link-source.txt"), raw("offline-link-added.txt")
        require(source in inventory and added in inventory, "owned offline links missing")
        require(inventory[source][0] == inventory[added][0], "owned offline links distinct objects")
        require({p for p, (obj, attr) in inventory.items() if obj == inventory[source][0]} == {source, added}, "offline object namespace differs")
        require(ident(actual[source][0], True) == ident(actual[added][0], True) == 2, "offline native link count")
        outer = [p for p in inventory if text(p).endswith("\\link-outer.txt")]
        inner = [p for p in inventory if text(p).endswith("\\new-dir\\link-inner.txt")]
        require(len(outer) == len(inner) == 1, "owned nested links missing")
        require(inventory[outer[0]][0] == inventory[inner[0]][0], "nested links distinct objects")
        require(ident(actual[outer[0]][0], True) == ident(actual[inner[0]][0], True) == 2, "nested native link count")
        result.update(offline_old_names_absent_new_names_present=True, deleted_hardlink_exact_old_name_absent=True,
                      offline_hardlink_same_object_exact_two_paths=True, offline_hardlink_native_link_count=2,
                      nested_and_outer_hardlink_same_native_object_two_links=True)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture")
    parser.add_argument("inventory")
    parser.add_argument("--verify-native-fixture", action="store_true")
    args = parser.parse_args()
    try:
        require(os.name == "nt", "Windows only")
        print(json.dumps(run(args), ensure_ascii=True))
    except (OSError, ValueError, struct.error) as error:
        # Do not print filesystem exception text that may expose unrelated names.
        print(json.dumps(dict(verified=False, error_category=type(error).__name__,
                              os_code=getattr(error, "winerror", None),
                              reason=str(error) if isinstance(error, ValueError) else "native or format failure")))
        raise SystemExit(1)
