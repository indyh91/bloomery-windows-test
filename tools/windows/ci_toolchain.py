"""Fetch the pinned LLVM and CUDA build inputs on a disposable Windows CI runner."""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import zipfile

LLVM = "https://github.com/llvm/llvm-project/releases/download/llvmorg-21.1.8/clang%2Bllvm-21.1.8-x86_64-pc-windows-msvc.tar.xz"
CUDA = "https://developer.download.nvidia.com/compute/cuda/redist/"
ROOT = Path(sys.argv[1])
ROOT.mkdir(parents=True, exist_ok=True)


def fetch(url, path, sha):
    subprocess.run(["curl.exe", "-fLsS", "--retry", "3", "-o", str(path), url], check=True)
    with path.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != sha:
        raise ValueError(f"{path.name} SHA-256 mismatch: {actual}")


def save(root, rel, stream):
    if not rel.parts or rel.is_absolute() or any(p in (".", "..") or ":" in p for p in rel.parts):
        raise ValueError(f"invalid archive path {rel}")
    destination = root.joinpath(*rel.parts)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("wb") as out:
        shutil.copyfileobj(stream, out)


def install_llvm():
    archive = ROOT / "llvm.tar.xz"
    fetch(LLVM, archive, "749d22f565fcd5718dbed06512572d0e5353b502c03fe1f7f17ee8b8aca21a47")
    programs = {"llc.exe", "llvm-link.exe", "llvm-as.exe", "llvm-dis.exe", "opt.exe", "clang.exe"}
    with tarfile.open(archive, mode="r|xz") as bundle:
        for entry in bundle:
            rel = PurePosixPath(*PurePosixPath(entry.name).parts[1:])
            if not entry.isfile():
                continue
            if (rel.parent == PurePosixPath("bin") and (rel.name in programs or rel.suffix == ".dll")) or rel.is_relative_to("lib/clang/21/include"):
                with bundle.extractfile(entry) as stream:
                    save(ROOT / "llvm", rel, stream)
    archive.unlink()


def install_cuda():
    manifest = ROOT / "redistrib.json"
    fetch(CUDA + "redistrib_13.3.0.json", manifest, "507eddaab1360336bc0fe17b77552e0b7dfe1e74da888671c3a2f5fad7775db1")
    versions = json.loads(manifest.read_bytes())
    for name in ("cuda_cudart", "cuda_crt", "cuda_nvcc", "libcurand", "libnvvm"):
        pkg = versions[name]["windows-x86_64"]
        archive = ROOT / (name + ".zip")
        fetch(CUDA + pkg["relative_path"], archive, pkg["sha256"])
        with zipfile.ZipFile(archive) as bundle:
            for entry in bundle.infolist():
                if entry.is_dir():
                    continue
                rel = PurePosixPath(*PurePosixPath(entry.filename).parts[1:])
                if rel.name == "libdevice.10.bc":
                    rel = PurePosixPath("nvvm/libdevice/libdevice.10.bc")
                elif not rel.is_relative_to("include") and rel.name != "ptxas.exe":
                    continue
                with bundle.open(entry) as stream:
                    save(ROOT / "cuda", rel, stream)
        archive.unlink()


if __name__ == "__main__":
    install_llvm()
    install_cuda()
    with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as env:
        print(f"CUDA_PATH={ROOT / 'cuda'}", file=env)
        print(f"CUDA_OXIDE_LLC={ROOT / 'llvm/bin/llc.exe'}", file=env)
