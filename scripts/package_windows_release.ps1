param(
  [Parameter(Mandatory = $true)]
  [string]$Version,

  [Parameter(Mandatory = $true)]
  [string]$Workspace,

  [Parameter(Mandatory = $true)]
  [string]$OutputDir
)

$ErrorActionPreference = "Stop"

$packageName = "cbz-tools-viewer-$Version-windows-x64"
$stageDir = Join-Path $OutputDir $packageName
$zipPath = Join-Path $OutputDir "$packageName.zip"
$shaPath = "$zipPath.sha256"
$vcpkgRoot = if ([string]::IsNullOrWhiteSpace($env:VCPKG_ROOT)) { "C:\vcpkg" } else { $env:VCPKG_ROOT }
$ffmpegShareDir = Join-Path $vcpkgRoot "installed/x64-windows/share/ffmpeg"

function Copy-File {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Source,

    [Parameter(Mandatory = $true)]
    [string]$Destination
  )

  $parent = Split-Path $Destination -Parent
  if ($parent) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
  }
  Copy-Item $Source $Destination -Force
}

if (Test-Path $stageDir) {
  Remove-Item $stageDir -Recurse -Force
}
if (Test-Path $zipPath) {
  Remove-Item $zipPath -Force
}
if (Test-Path $shaPath) {
  Remove-Item $shaPath -Force
}

New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

Copy-File (Join-Path $Workspace "target/release/cbz-viewer.exe") (Join-Path $stageDir "cbz-viewer.exe")

$runtimeDir = Join-Path $Workspace "target/release"
$corePath = Join-Path $runtimeDir "cbz-viewer-core.exe"
if (!(Test-Path -LiteralPath $corePath -PathType Leaf)) {
  throw "The core executable used as the launcher's embedded input is missing: $corePath"
}
if (!(Test-Path -LiteralPath (Join-Path $runtimeDir "dav1d.dll") -PathType Leaf)) {
  throw "The dav1d runtime used as the launcher's embedded input is missing"
}
$allowedFfmpegRegex = '^(avcodec|avformat|avutil|swresample|swscale)-\d+\.dll$'
$ffmpegDlls = @(Get-ChildItem -LiteralPath $runtimeDir -File -ErrorAction SilentlyContinue |
  Where-Object { $_.Name -match '^(av|sw).+\.dll$' })
$unexpectedFfmpegDlls = @($ffmpegDlls | Where-Object { $_.Name -notmatch $allowedFfmpegRegex })
if ($unexpectedFfmpegDlls.Count -gt 0) {
  throw "Unexpected FFmpeg runtime DLLs in release directory: $($unexpectedFfmpegDlls.Name -join ', ')"
}

foreach ($component in @('avcodec', 'avformat', 'avutil', 'swresample', 'swscale')) {
  $matches = @(Get-ChildItem -LiteralPath $runtimeDir -Filter "$component-*.dll" -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match $allowedFfmpegRegex })
  if ($matches.Count -ne 1) {
    throw "Expected exactly one staged FFmpeg runtime DLL for $component, found $($matches.Count)"
  }
}

$rootDlls = @(Get-ChildItem -LiteralPath $stageDir -File -ErrorAction SilentlyContinue |
  Where-Object { $_.Extension -ieq ".dll" })
if ($rootDlls.Count -gt 0) {
  throw "Runtime DLLs must remain embedded by the launcher and absent from the package root: $($rootDlls.Name -join ', ')"
}

Copy-File (Join-Path $Workspace "README.md") (Join-Path $stageDir "README.md")
Copy-File (Join-Path $Workspace "README.ja.md") (Join-Path $stageDir "README.ja.md")
Copy-File (Join-Path $Workspace "THIRDPARTY_LICENSES.md") (Join-Path $stageDir "THIRDPARTY_LICENSES.md")
Copy-File (Join-Path $Workspace "LICENSE") (Join-Path $stageDir "LICENSE")
Copy-File (Join-Path $Workspace "third_party/dav1d/LICENSE") (Join-Path $stageDir "third_party/dav1d/LICENSE")
Copy-File (Join-Path $ffmpegShareDir "copyright") (Join-Path $stageDir "third_party/ffmpeg/copyright")
Copy-File (Join-Path $ffmpegShareDir "vcpkg.spdx.json") (Join-Path $stageDir "third_party/ffmpeg/vcpkg.spdx.json")

Compress-Archive -Path $stageDir -DestinationPath $zipPath

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
try {
  $packageRoot = "$packageName/"
  $rootDllEntries = @($archive.Entries | Where-Object {
    $_.FullName.StartsWith($packageRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
      $_.FullName.Substring($packageRoot.Length) -notmatch '/' -and
      $_.Name -match '\.dll$'
  })
  if ($rootDllEntries.Count -gt 0) {
    throw "Release ZIP contains DLLs at its root: $($rootDllEntries.FullName -join ', ')"
  }
}
finally {
  $archive.Dispose()
}

$hash = Get-FileHash -Algorithm SHA256 -Path $zipPath
"$($hash.Hash.ToLowerInvariant())  $($hash.Path | Split-Path -Leaf)" | Set-Content -Path $shaPath
