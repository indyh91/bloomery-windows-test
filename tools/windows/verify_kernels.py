"""Assemble the CUDA modules actually embedded in the Windows release executable."""
from pathlib import Path
import struct
import subprocess
import sys
import tempfile


def oxart(executable):
    data = executable.read_bytes()
    pe, = struct.unpack_from("<I", data, 60)
    if data[:2] != b"MZ" or data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("expected a PE executable")
    machine, count = struct.unpack_from("<HH", data, pe + 4)
    optional, = struct.unpack_from("<H", data, pe + 20)
    if machine != 0x8664:
        raise ValueError("expected a Windows x64 executable")
    for i in range(count):
        start = pe + 24 + optional + i * 40
        if data[start:start + 8].rstrip(b"\0") == b".oxart":
            virtual_size, _, raw_size, raw_at = struct.unpack_from("<IIII", data, start + 8)
            return data[raw_at:raw_at + min(raw_size, virtual_size)]
    raise ValueError("missing .oxart in the linked executable")


def verify(executable, assembler, directory):
    section = oxart(executable)
    offset = 0
    modules = {}
    while offset < len(section):
        if section[offset:offset + 8] != b"OXIDEART":
            if not any(section[offset:]):
                break
            raise ValueError(f"invalid CUDA bundle at offset {offset}")
        _, header, length, namesz, archsz, count, entries = struct.unpack_from("<HHIHHHH", section, offset + 8)
        if length < header or offset + length > len(section):
            raise ValueError("invalid CUDA bundle length")
        blob = section[offset:offset + length]
        offset += length
        name = blob[header:header + namesz].decode()
        arch = blob[header + namesz:header + namesz + archsz].decode()
        if arch != "sm_86" or name in modules:
            raise ValueError(f"unexpected bundle {name} ({arch})")
        records = header + namesz + archsz
        for i in range(count):
            kind, _, pos, size = struct.unpack_from("<HHII", blob, records + i * 24)
            if kind != 0x100:
                continue
            ptx = blob[pos:pos + size].rstrip(b"\0")
            source = directory / "module.ptx"
            source.write_bytes(ptx)
            subprocess.run([str(assembler), "-arch=sm_86", str(source), "-o", str(directory / "module.cubin")], check=True)
            modules[name] = entries
            print(f"{name} ({arch}): {entries} entries", flush=True)
    if not modules.get("bloomery-gpu"):
        raise ValueError("missing shared ERNIE CUDA kernels")
    print(f"Assembled {len(modules)} modules with {sum(modules.values())} entries for SM 8.6")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory() as tmp:
        verify(Path(sys.argv[1]), Path(sys.argv[2]).resolve(), Path(tmp))
