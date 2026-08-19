$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Push-Location $repoRoot
try {
    Write-Host "Release: building cbz-viewer-core..."
    & cargo build --locked --release --bin cbz-viewer-core
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    Write-Host "Release: building cbz-viewer-launcher..."
    & cargo build --locked --release -p cbz-viewer-launcher
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    Write-Host "Release build complete."
}
finally {
    Pop-Location
}
