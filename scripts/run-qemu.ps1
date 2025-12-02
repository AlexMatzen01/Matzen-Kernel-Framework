param(
    [string]$Artifact = "target/x86_64-mfk/debug/bootimage-mfk-kernel.bin",
    [string]$Qemu = "qemu-system-x86_64"
)

if (-not (Test-Path $Artifact)) {
    Write-Error "Artifact $Artifact not found. Run 'cargo bootimage -p mfk-kernel' first."
    exit 1
}

& $Qemu `
    -drive format=raw,file=$Artifact `
    -serial stdio `
    -display none `
    -m 256M `
    -cpu qemu64 `
    -smp 2 `
    -no-reboot `
    -d int
