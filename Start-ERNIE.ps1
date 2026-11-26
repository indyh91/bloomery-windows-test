[CmdletBinding()]
param(
    [Parameter(Mandatory, Position=0)][string]$Model,
    [ValidateRange(1,2147483647)][int]$Context,
    [ValidateRange(1,65535)][int]$Port = 8091,
    [ValidateRange(0,255)][int]$Gpu,
    [string]$Binary,
    [switch]$Plan
)
$ErrorActionPreference = 'Stop'
if (!$Binary) {
    $Binary = @(
        (Join-Path $PSScriptRoot 'bloomery-serve.exe')
        (Join-Path $PSScriptRoot 'target\x86_64-pc-windows-msvc\release\bloomery-serve.exe')
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if (!$Binary) { throw 'Extract the Windows binary beside Start-ERNIE.ps1 or run tools\windows\Build-ERNIE.ps1.' }
}
$exe = (Resolve-Path -LiteralPath $Binary).ProviderPath
$weights = (Resolve-Path -LiteralPath $Model).ProviderPath
$devices = & nvidia-smi.exe --query-gpu=index,name,uuid --format=csv,noheader
if ($LASTEXITCODE -ne 0) { throw 'nvidia-smi could not enumerate the NVIDIA GPUs.' }
$gpus = @(ConvertFrom-Csv -InputObject $devices -Header Index,Name,Uuid)
$selected = if ($PSBoundParameters.ContainsKey('Gpu')) {
    @($gpus | Where-Object { [int]$_.Index -eq $Gpu })
} else {
    @($gpus | Where-Object { $_.Name.Trim() -match '\bRTX 3090$' })
}
if (@($selected).Count -ne 1 -or !$selected) { throw 'Select one device with -Gpu and its nvidia-smi index. Automatic selection requires one RTX 3090.' }
$vars = @{
    CUDA_DEVICE_ORDER = 'PCI_BUS_ID'
    CUDA_VISIBLE_DEVICES = $selected.Uuid.Trim()
    BLOOMERY_HOST_POPULATE = '1'
    BLOOMERY_HOST_LOCK = '1'
}
$old = @{}
foreach ($key in $vars.Keys) { $old[$key] = [Environment]::GetEnvironmentVariable($key, 'Process') }
try {
    foreach ($key in $vars.Keys) { [Environment]::SetEnvironmentVariable($key,$vars[$key],'Process') }
    $options = @('--model','ernie55','-m',$weights,'--place','a','--host','127.0.0.1','--port',[string]$Port,'--parallel','1')
    if ($PSBoundParameters.ContainsKey('Context')) { $options += @('--ctx',[string]$Context) }
    if ($Plan) { $options += @('--plan','--json') }
    & $exe @options
    $code = $LASTEXITCODE
} finally {
    foreach ($key in $vars.Keys) { [Environment]::SetEnvironmentVariable($key,$old[$key],'Process') }
}
exit $code
