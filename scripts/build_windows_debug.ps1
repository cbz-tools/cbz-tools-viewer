$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Push-Location $repoRoot
try {
    Write-Host "Debug: building cbz-viewer-core..."
    & cargo build --locked --bin cbz-viewer-core
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    Write-Host "Debug: building cbz-viewer-launcher..."
    & cargo build --locked -p cbz-viewer-launcher
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    Write-Host "Debug build complete."
}
finally {
    Pop-Location
}
