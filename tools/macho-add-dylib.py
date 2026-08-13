#!/usr/bin/env python3
"""Add LC_LOAD_DYLIB to decrypted arm64 Mach-O slices using existing header slack."""

from __future__ import annotations

import argparse
import pathlib
import struct
import sys

MH_MAGIC_64 = 0xFEEDFACF
FAT_MAGIC = 0xCAFEBABE
FAT_MAGIC_64 = 0xCAFEBABF
CPU_TYPE_ARM64 = 0x0100000C
LC_SEGMENT_64 = 0x19
LC_LOAD_DYLIB = 0x0C
LC_ENCRYPTION_INFO = 0x21
LC_ENCRYPTION_INFO_64 = 0x2C


class PatchError(RuntimeError):
    pass


def align(value: int, boundary: int) -> int:
    return (value + boundary - 1) & ~(boundary - 1)


def dylib_command(path: str) -> bytes:
    encoded = path.encode("utf-8") + b"\0"
    size = align(24 + len(encoded), 8)
    return struct.pack("<IIIIII", LC_LOAD_DYLIB, size, 24, 0, 0, 0) + encoded.ljust(size - 24, b"\0")


def patch_slice(data: bytearray, base: int, size: int, install_name: str) -> bool:
    if size < 32 or struct.unpack_from("<I", data, base)[0] != MH_MAGIC_64:
        raise PatchError(f"slice at 0x{base:x} is not a little-endian Mach-O 64 image")
    ncmds, sizeofcmds = struct.unpack_from("<II", data, base + 16)
    command_start = base + 32
    command_end = command_start + sizeofcmds
    slice_end = base + size
    if command_end > slice_end:
        raise PatchError("load commands extend outside Mach-O slice")

    cursor = command_start
    first_section_offset = size
    for _ in range(ncmds):
        if cursor + 8 > command_end:
            raise PatchError("truncated load command")
        command, command_size = struct.unpack_from("<II", data, cursor)
        if command_size < 8 or cursor + command_size > command_end:
            raise PatchError("invalid load command size")
        if command == LC_LOAD_DYLIB and command_size >= 24:
            name_offset = struct.unpack_from("<I", data, cursor + 8)[0]
            if 0 < name_offset < command_size:
                raw = data[cursor + name_offset : cursor + command_size].split(b"\0", 1)[0]
                if raw.decode("utf-8", errors="replace") == install_name:
                    return False
        if command in (LC_ENCRYPTION_INFO, LC_ENCRYPTION_INFO_64) and command_size >= 20:
            cryptid = struct.unpack_from("<I", data, cursor + 16)[0]
            if cryptid != 0:
                raise PatchError("input executable is encrypted; provide a legally decrypted IPA")
        if command == LC_SEGMENT_64 and command_size >= 72:
            nsects = struct.unpack_from("<I", data, cursor + 64)[0]
            section = cursor + 72
            for _ in range(nsects):
                if section + 80 > cursor + command_size:
                    raise PatchError("truncated section_64")
                offset = struct.unpack_from("<I", data, section + 48)[0]
                if offset:
                    first_section_offset = min(first_section_offset, offset)
                section += 80
        cursor += command_size

    command = dylib_command(install_name)
    relative_end = 32 + sizeofcmds
    if relative_end + len(command) > first_section_offset:
        raise PatchError(
            f"not enough Mach-O header slack (need {len(command)} bytes, "
            f"have {max(0, first_section_offset - relative_end)})"
        )
    insertion = base + relative_end
    if any(data[insertion : insertion + len(command)]):
        raise PatchError("Mach-O header slack is not zero-filled; refusing to overwrite data")
    data[insertion : insertion + len(command)] = command
    struct.pack_into("<II", data, base + 16, ncmds + 1, sizeofcmds + len(command))
    return True


def slices(data: bytearray) -> list[tuple[int, int]]:
    if len(data) < 4:
        raise PatchError("file is too small")
    little = struct.unpack_from("<I", data, 0)[0]
    if little == MH_MAGIC_64:
        cpu_type = struct.unpack_from("<I", data, 4)[0]
        if cpu_type != CPU_TYPE_ARM64:
            raise PatchError("thin executable is not arm64")
        return [(0, len(data))]

    big = struct.unpack_from(">I", data, 0)[0]
    if big not in (FAT_MAGIC, FAT_MAGIC_64):
        raise PatchError("unsupported Mach-O or FAT magic")
    count = struct.unpack_from(">I", data, 4)[0]
    entry_size = 20 if big == FAT_MAGIC else 32
    result = []
    for index in range(count):
        cursor = 8 + index * entry_size
        if cursor + entry_size > len(data):
            raise PatchError("truncated FAT header")
        cpu_type = struct.unpack_from(">I", data, cursor)[0]
        if big == FAT_MAGIC:
            offset, size = struct.unpack_from(">II", data, cursor + 8)
        else:
            offset, size = struct.unpack_from(">QQ", data, cursor + 8)
        if cpu_type == CPU_TYPE_ARM64:
            result.append((offset, size))
    if not result:
        raise PatchError("FAT executable has no arm64 slice")
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("executable", type=pathlib.Path)
    parser.add_argument("install_name")
    args = parser.parse_args()
    data = bytearray(args.executable.read_bytes())
    changed = False
    for offset, size in slices(data):
        if offset + size > len(data):
            raise PatchError("FAT slice extends beyond file")
        changed |= patch_slice(data, offset, size, args.install_name)
    if changed:
        temporary = args.executable.with_name(args.executable.name + ".isaaccloud.tmp")
        temporary.write_bytes(data)
        temporary.chmod(args.executable.stat().st_mode)
        temporary.replace(args.executable)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except PatchError as error:
        print(f"macho-add-dylib: {error}", file=sys.stderr)
        raise SystemExit(1)
