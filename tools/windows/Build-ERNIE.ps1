[CmdletBinding()]
param([string]$CudaPath = $env:CUDA_PATH, [string]$Llc = $env:CUDA_OXIDE_LLC)
$ErrorActionPreference = 'Stop'

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program exited with code $LASTEXITCODE" }
}

$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
Push-Location -LiteralPath $repo
try {
    if (-not [Environment]::Is64BitProcess -or $env:OS -ne 'Windows_NT') { throw 'Run in 64-bit PowerShell on Windows.' }
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
        $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
        if (!(Test-Path -LiteralPath $vswhere)) { throw 'Install the Visual Studio x64 C++ Build Tools and Windows SDK.' }
        $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if (!$vs) { throw 'Visual Studio x64 C++ Build Tools were not found.' }
        Import-Module (Join-Path $vs 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
        Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64' | Out-Null
    }
    if (!$CudaPath) { $CudaPath = $env:CUDA_TOOLKIT_PATH }
    if (!$CudaPath) { throw 'Supply -CudaPath pointing to CUDA 13.3 or later with cuRAND headers.' }
    $cudaHeader = Join-Path $CudaPath 'include\cuda.h'
    if (!(Test-Path $cudaHeader) -or !(Test-Path (Join-Path $CudaPath 'include\curand.h'))) { throw "Missing cuda.h or curand.h under $CudaPath\include" }
    if (!(Select-String -Quiet -LiteralPath $cudaHeader -Pattern 'CU_STREAM_MEM_OP_ATOMIC_REDUCTION')) {
        throw 'The build requires the stream atomic-reduction declarations in the CUDA 13.3 or later headers.'
    }
    $env:CUDA_TOOLKIT_PATH = (Resolve-Path -LiteralPath $CudaPath).Path
    if (!$Llc) { $Llc = (Get-Command llc.exe -ErrorAction Stop).Source }
    $env:CUDA_OXIDE_LLC = (Get-Command $Llc -ErrorAction Stop).Source
    $llvmVersion = (& $env:CUDA_OXIDE_LLC --version) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $llvmVersion -notmatch 'LLVM version 21\.' -or $llvmVersion -notmatch 'nvptx64') {
        throw 'Supply -Llc pointing to LLVM 21 llc.exe with NVPTX support.'
    }
    $llvmBin = Split-Path $env:CUDA_OXIDE_LLC
    $llvmLink = Join-Path $llvmBin 'llvm-link.exe'
    if (!(Test-Path $llvmLink) -or ((& $llvmLink --version) -join ' ') -notmatch 'LLVM version 21\.') {
        throw 'Install llvm-link.exe from LLVM 21 alongside llc.exe for CUDA libdevice linking.'
    }
    $env:CUDA_OXIDE_LIBDEVICE = (Resolve-Path -LiteralPath (Join-Path $env:CUDA_TOOLKIT_PATH 'nvvm\libdevice\libdevice.10.bc')).Path
    if (!$env:LIBCLANG_PATH) { $env:LIBCLANG_PATH = $llvmBin }
    $env:Path = "$llvmBin;$env:LIBCLANG_PATH;$env:CUDA_TOOLKIT_PATH\bin\x64;$env:CUDA_TOOLKIT_PATH\bin;$env:Path"
    if (!$env:LIBFFI_LIB_DIR) {
        $roots = @($env:VCPKG_INSTALLATION_ROOT, $env:VCPKG_ROOT) | Where-Object { $_ }
        $vcpkg = $roots | ForEach-Object { Join-Path $_ 'vcpkg.exe' } | Where-Object { Test-Path $_ } | Select-Object -First 1
        if (!$vcpkg) { $vcpkg = (Get-Command vcpkg.exe -ErrorAction Stop).Source }
        $env:VCPKG_ROOT = Split-Path $vcpkg
        $manifest = Join-Path $repo 'tools\windows'
        $installed = Join-Path $repo 'target\windows-tools\vcpkg'
        Invoke-Checked $vcpkg @('install', "--x-manifest-root=$manifest", "--x-install-root=$installed", '--triplet=x64-windows')
        $env:LIBFFI_LIB_DIR = Join-Path $installed 'x64-windows\lib'
    }
    $env:Path = "$(Split-Path $env:LIBFFI_LIB_DIR)\bin;$env:Path"
    $channel = [regex]::Match((Get-Content rust-toolchain.toml -Raw), 'channel\s*=\s*"([^"]+)"').Groups[1].Value
    $rev = [regex]::Match((Get-Content Cargo.toml -Raw), '(?m)^cuda-device = .*rev = "([0-9a-f]{40})"').Groups[1].Value
    if (!$channel -or !$rev) { throw 'Missing pinned Rust channel or CUDA-Oxide revision.' }
    $env:RUSTUP_TOOLCHAIN = "$channel-x86_64-pc-windows-msvc"
    Invoke-Checked 'rustup' @('toolchain','install',$env:RUSTUP_TOOLCHAIN,'--profile','minimal','--component','rust-src,rustc-dev,llvm-tools')
    $env:RUSTC_BOOTSTRAP = '1'
    $install = Join-Path $repo "target\windows-tools\$rev"
    $oxide = Join-Path $install 'bin\cargo-oxide.exe'
    if (!(Test-Path -LiteralPath $oxide)) {
        Invoke-Checked 'cargo' @('install','--locked','--git','https://github.com/NVIDIA/cuda-rust.git','--rev',$rev,'--root',$install,'cargo-oxide')
    }
    $env:Path = "$(Split-Path $oxide);$env:Path"
    # cargo-oxide forwards RUSTFLAGS to the host compiler. Read the target's flags from the workspace.
    $config = Get-Content .cargo/config.toml -Raw
    $flags = [regex]::Match($config,'(?ms)^\[target\.x86_64-pc-windows-msvc\][^\[]*?rustflags\s*=\s*(\[[^\]]+\])').Groups[1].Value
    if (!$flags) { throw 'Missing Windows host rustflags in .cargo/config.toml.' }
    $env:RUSTFLAGS = (ConvertFrom-Json $flags) -join ' '
    $env:CARGO_BUILD_TARGET = 'x86_64-pc-windows-msvc'
    $features = 'glm5next,clef,mimo2,solar2,minimaxm3,flint,kimik3,hy4,ernie55,mistrallarge4,gemma-embedding2,hf,checkpoint'
    Invoke-Checked 'cargo' @('oxide','build','--arch','sm_86','--','--locked','--release','-p','bloomery-gpu-gates','--features',$features,'--bin','bloomery-serve','--target',$env:CARGO_BUILD_TARGET)
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    $binary = Join-Path $target 'x86_64-pc-windows-msvc\release\bloomery-serve.exe'
    Invoke-Checked $binary @('--version')
    Write-Output "Built $binary"
} finally {
    Pop-Location
}
