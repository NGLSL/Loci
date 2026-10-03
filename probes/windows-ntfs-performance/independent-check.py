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

MAX_BYTES = 8 * 1024 * 1024
MAX_ENTRIES = 32768


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def raw(s):
    b = s.encode("utf-16-le", "surrogatepass")
    return tuple(struct.unpack("<" + "H" * (len(b) // 2), b))


def text(units):
    return struct.pack("<" + "H" * len(units), *units).decode("utf-16-le", "surrogatepass")


PINS = []


def pin(path, directory):
    share = 3 if directory else 1
    handle = k.CreateFileW(str(path), 1, share, None, 3, 0x02200000, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    info = INFO()
    if not k.GetFileInformationByHandle(handle, ctypes.byref(info)):
        code = ctypes.get_last_error()
        k.CloseHandle(handle)
        raise ctypes.WinError(code)
    if info.attributes & 0x400 or bool(info.attributes & 0x10) != directory:
        k.CloseHandle(handle)
        raise ValueError("pinned engineering path has wrong type or is reparse")
    PINS.append(handle)


def reject_reparse_chain(selected):
    # Inspect from drive root downward: never traverse a later component before
    # rejecting an alternate target in an earlier engineering-path component.
    for component in [*reversed(selected.parents), selected]:
        pin(component, component != selected or selected.name != "inventory.lcusn")


def selected_paths(fixture, inventory):
    literal_script = Path(os.path.abspath(__file__))
    literal_run = literal_script.parents[2] / ".scratch/windows-ntfs-performance/run"
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
    require(76 <= len(b) <= MAX_BYTES, "snapshot byte budget")
    require(b[:8] == b"LCUSNB2\0", "snapshot magic")
    require(struct.unpack_from("<II", b, 8) == (2, len(b)), "version or declared format length")
    checksum = 0xcbf29ce484222325
    for unit in b[:-8]:
        checksum = ((checksum ^ unit) * 0x100000001b3) & ((1 << 64) - 1)
    require(checksum == int.from_bytes(b[-8:], "little"), "snapshot checksum")
    volume, journal, cursor = struct.unpack_from("<QQq", b, 16)
    root_id = int.from_bytes(b[40:56], "little")
    scope_len, count, guid_len = struct.unpack_from("<III", b, 56)
    require(cursor >= 0 and root_id != 0, "cursor or root identity")
    require(0 < scope_len <= 32768 and count <= MAX_ENTRIES, "scope/inventory budget")
    end = len(b) - 8
    require(68 + scope_len * 2 + guid_len <= end, "truncated scope")
    scope = struct.unpack_from("<" + "H" * scope_len, b, 68)
    expected = str(fixture)
    if not expected.startswith("\\\\?\\"):
        require(len(expected) > 2 and expected[1:3] == ":\\", "fixture must use drive-backed path")
        expected = "\\\\?\\" + expected
    require(scope == raw(expected), "scope differs from explicit fixture; refuse traversal")
    require(0 not in scope, "scope NUL")
    entries, objects, directories, namespace = [], {}, {}, set()
    require(0 < guid_len <= 128, "volume GUID length")
    guid_start = 68 + scope_len * 2
    guid = b[guid_start:guid_start + guid_len].decode("ascii")
    import re
    require(re.fullmatch(r"\\\\\?\\Volume\{[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\}\\", guid) is not None, "volume GUID format")
    offset = guid_start + guid_len
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
    depths = {root_id: 0}
    prefix_bytes = 0
    directory_names = {obj: name for parent, obj, name, attr in entries if attr & 16}
    remaining = dict(directories)
    while remaining:
        progressed = False
        for obj, parent in list(remaining.items()):
            if parent in paths:
                paths[obj] = paths[parent] + ((92,) if paths[parent] else ()) + directory_names[obj]
                depths[obj] = depths[parent] + 1
                require(depths[obj] <= 64, "directory depth budget")
                prefix_bytes += len(paths[obj]) * 2
                require(prefix_bytes <= 32 * 1024 * 1024, "directory prefix memory budget")
                del remaining[obj]
                progressed = True
        require(progressed, "directory cycle or unrooted graph")
    inventory = {paths[parent] + ((92,) if paths[parent] else ()) + name: (obj, attr) for parent, obj, name, attr in entries}
    require(sum(len(p) * 2 for p in inventory) <= 32 * 1024 * 1024, "derived paths memory budget")
    require(len(inventory) == count, "duplicate resolved paths")
    return text(scope), inventory, volume, journal, cursor, root_id, len(b), guid


def scan(root):
    actual = {}
    pending = [(root, (), 0)]
    path_bytes = 0
    while pending:
        directory, relative, depth = pending.pop()
        require(depth <= 64, "actual depth budget")
        with os.scandir(directory) as iterator:
            for entry in iterator:
                rel = relative + ((92,) if relative else ()) + raw(entry.name)
                info = entry.stat(follow_symlinks=False)
                require(len(actual) < MAX_ENTRIES, "actual fixture exceeds entry budget")
                require(len(rel) <= 32768, "actual path exceeds raw-name budget")
                path_bytes += len(rel) * 2
                require(path_bytes <= 32 * 1024 * 1024, "actual derived paths memory budget")
                actual[rel] = (entry.path, info)
                if info.st_file_attributes & 16 and not info.st_file_attributes & 0x400:
                    pending.append((entry.path, rel, depth + 1))
    return actual


k = ctypes.WinDLL("kernel32", use_last_error=True)
k.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32,
                         ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
k.CreateFileW.restype = ctypes.c_void_p
k.CloseHandle.argtypes = [ctypes.c_void_p]
k.CloseHandle.restype = ctypes.c_int
k.GetVolumeNameForVolumeMountPointW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32]
k.GetVolumeNameForVolumeMountPointW.restype = ctypes.c_int


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
    root, inventory, volume, journal, cursor, root_id, file_bytes, guid = decode(inventory_file, fixture)
    # Scope was validated before native root access or directory traversal.
    require(ident(root) == (volume, root_id), "root native identity differs from persisted scope")
    guid_buffer = ctypes.create_unicode_buffer(128)
    require(k.GetVolumeNameForVolumeMountPointW(os.path.splitdrive(str(fixture))[0] + "\\", guid_buffer, 128), "native volume GUID query failed")
    if args.simulated_checkpoint:
        require(not args.verify_native_fixture, "simulated checkpoint cannot assert native recovery")
        require(journal == cursor == 0 and guid == "\\\\?\\Volume{00000000-0000-0000-0000-000000000001}\\", "only explicit ordinary scan simulated checkpoint is accepted")
    else:
        require(guid_buffer.value.lower() == guid.lower(), "persisted volume GUID differs from native mount identity")
    actual = scan(root)
    require(set(inventory) == set(actual), "full raw UTF16 path set mismatch")
    for rel, (path, info) in actual.items():
        require(ident(path) == (volume, inventory[rel][0]), "native entry identity mismatch")
        require(info.st_file_attributes == inventory[rel][1], "native attributes mismatch")
    second = scan(root)
    require(set(second) == set(actual), "fixture paths changed during crosscheck")
    for rel, (path, info) in second.items():
        require(ident(path) == (volume, inventory[rel][0]) and info.st_file_attributes == inventory[rel][1], "fixture identities or attributes changed during crosscheck")
    result = dict(full_raw_utf16_set_equal=True, snapshot_entries=len(inventory), independent_traversal_entries=len(actual),
                  all_native_legacy_volume_object_attributes_equal=True, scope_exact_checked_before_traversal=True,
                  root_native_identity_guard=True, raw_unpaired_surrogate_retained=any(0xd800 in p for p in actual),
                  volume_guid_native_equal=not args.simulated_checkpoint, checkpoint_simulated=args.simulated_checkpoint, checkpoint_checksum_valid=True, snapshot_file_bytes=file_bytes, journal_id=journal,
                  usn_cursor=cursor, million_file_monitoring_proven=False)
    if args.count is not None:
        synthetic_files = [p for p, (_, info) in actual.items() if text(p).startswith("bucket-") and not info.st_file_attributes & 16]
        require(len(synthetic_files) == args.count, "real fixture population differs from requested count")
        result["real_population_files"] = len(synthetic_files)
    if args.verify_native_fixture:
        for old in ["offline-delete.txt", "offline-old.txt", "offline-dir", "offline-old-link.txt"]:
            require(raw(old) not in actual, "owned fixture old name survived")
        for new in ["offline-added.txt", "offline-new.txt", "offline-dir-new"]:
            require(raw(new) in actual, "owned fixture new name missing")
        source, added = raw("offline-source.txt"), raw("offline-new-link.txt")
        require(source in inventory and added in inventory, "owned offline links missing")
        require(inventory[source][0] == inventory[added][0], "owned offline links distinct objects")
        require({p for p, (obj, attr) in inventory.items() if obj == inventory[source][0]} == {source, added}, "offline object namespace differs")
        require(ident(actual[source][0], True) == ident(actual[added][0], True) == 2, "offline native link count")
        require(raw("offline-move-out") not in actual, "offline move-out root survived")
        require(raw("offline-move-in\\childin.txt") in actual, "offline incoming child missing")
        result.update(offline_old_names_absent_new_names_present=True, deleted_hardlink_exact_old_name_absent=True,
                      offline_hardlink_same_object_exact_two_paths=True, offline_hardlink_native_link_count=2,
                      offline_scope_boundary_directory_moves_verified=True)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture")
    parser.add_argument("inventory")
    parser.add_argument("--simulated-checkpoint", action="store_true", help="Only ordinary acceptance-scan journal=cursor=0 with fixed synthetic GUID; never native USN proof")
    parser.add_argument("--count", type=int, choices=[1000, 10000])
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
    finally:
        for handle in reversed(PINS):
            k.CloseHandle(handle)
