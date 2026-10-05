# Hyper-V COM1 serial capture helper for MFK (Generation 2, UEFI).
# The runner maps VM COM1 to \\.\pipe\<PipeName>; this script connects to
# that named pipe and tees output to the console + target/mfk-hyperv-serial.log.
# Requires: Hyper-V VM already created/running via `.\run.ps1 --hyperv --uefi`.
param(
    [string]$VMName = "",
    [string]$PipeName = "",
    [string]$LogPath = "target/mfk-hyperv-serial.log"
)

$ErrorActionPreference = "Stop"

function Resolve-VMName([string]$hint) {
    if ($hint -ne "") { return $hint }
    $vms = @(Get-VM -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "MFK-*-uefi" } | Select-Object -ExpandProperty Name)
    if ($vms.Count -eq 1) { return $vms[0] }
    if ($vms.Count -gt 1) {
        Write-Host "Multiple MFK VMs found:" -ForegroundColor Yellow
        $vms | ForEach-Object { Write-Host "  $_" }
        throw "Pass -VMName explicitly."
    }
    throw "No MFK-*-uefi VM found. Create one first: .\\run.ps1 --hyperv --uefi"
}

function Resolve-PipeName([string]$vm, [string]$hint) {
    if ($hint -ne "") { return ($hint -replace "^\\\\\.\\pipe\\", "") }
    try {
        $com = Get-VMComPort -VMName $vm -Number 1 -ErrorAction SilentlyContinue
        if ($com -and $com.Path -match "pipe\\(.+)$") { return $Matches[1] }
    } catch { }
    # Runner default: MFK-<vm-suffix>-com1 where vm = MFK-<suffix>-uefi.
    $suffix = $vm -replace "^MFK-", "" -replace "-uefi$", ""
    return "MFK-$suffix-com1"
}

try {
    $VMName = Resolve-VMName $VMName
    $PipeName = Resolve-PipeName $VMName $PipeName
} catch {
    Write-Host "ERROR: $_" -ForegroundColor Red
    exit 1
}

$pipePath = "\\.\\pipe\\$PipeName"
Write-Host "Connecting to $pipePath (VM: $VMName)..." -ForegroundColor Cyan
Write-Host "Log: $LogPath (Ctrl+C to stop, VM keeps running)" -ForegroundColor Gray

$logDir = Split-Path -Parent $LogPath
if ($logDir -ne "" -and -not (Test-Path -LiteralPath $logDir)) {
    New-Item -ItemType Directory -Path $logDir -Force | Out-Null
}

try {
    $client = New-Object System.IO.Pipes.NamedPipeClientStream(".", $PipeName, [System.IO.Pipes.PipeDirection]::In)
    $client.Connect(10000)
} catch {
    Write-Host "ERROR: Could not connect to $pipePath" -ForegroundColor Red
    Write-Host "Is the VM running? Check: Get-VM -Name '$VMName' | Select State" -ForegroundColor Yellow
    Write-Host "Check COM path: Get-VMComPort -VMName '$VMName'" -ForegroundColor Yellow
    exit 1
}

$reader = New-Object System.IO.StreamReader($client)
$log = [System.IO.StreamWriter]::new($LogPath, $true)
try {
    Write-Host "Connected. Streaming serial output..." -ForegroundColor Green
    while ($client.IsConnected) {
        $line = $reader.ReadLine()
        if ($null -eq $line) { break }
        Write-Host $line
        $log.WriteLine($line)
        $log.Flush()
    }
} finally {
    $log.Close()
    $reader.Close()
    $client.Close()
    Write-Host ""
    Write-Host "Disconnected. Log saved: $LogPath" -ForegroundColor Cyan
}
