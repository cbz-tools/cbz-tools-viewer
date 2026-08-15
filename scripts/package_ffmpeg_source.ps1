param(
  [Parameter(Mandatory = $true)]
  [string]$Version,

  [Parameter(Mandatory = $true)]
  [string]$VcpkgRoot,

  [Parameter(Mandatory = $true)]
  [string]$OutputDir,

  [string]$Triplet = "x64-windows"
)

$ErrorActionPreference = "Stop"

function Get-FullPath {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path
  )

  return [System.IO.Path]::GetFullPath($Path)
}

function Copy-DirectoryContents {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Source,

    [Parameter(Mandatory = $true)]
    [string]$Destination
  )

  New-Item -ItemType Directory -Force -Path $Destination | Out-Null
  foreach ($item in Get-ChildItem -LiteralPath $Source -Force) {
    Copy-Item -LiteralPath $item.FullName -Destination $Destination -Recurse -Force
  }
}

function Get-ManifestVersion {
  param(
    [Parameter(Mandatory = $true)]
    [object]$Manifest,

    [Parameter(Mandatory = $true)]
    [string]$ManifestPath
  )

  foreach ($field in @("version", "version-string", "version-semver")) {
    if ($Manifest.PSObject.Properties.Name -contains $field) {
      $value = [string]$Manifest.$field
      if (![string]::IsNullOrWhiteSpace($value)) {
        return $value
      }
    }
  }

  throw "The FFmpeg port version is missing from $ManifestPath"
}

function Get-PortValue {
  param(
    [Parameter(Mandatory = $true)]
    [string]$PortFileText,

    [Parameter(Mandatory = $true)]
    [string]$Name
  )

  $pattern = '(?m)^\s*' + [regex]::Escape($Name) + '\s+(?<value>"[^"]+"|\S+)'
  $match = [regex]::Match($PortFileText, $pattern)
  if (!$match.Success) {
    throw "The FFmpeg portfile does not contain $Name metadata"
  }
  return $match.Groups["value"].Value.Trim('"')
}

$vcpkgRoot = Get-FullPath $VcpkgRoot
$outputDir = Get-FullPath $OutputDir
$vcpkgExe = Join-Path $vcpkgRoot "vcpkg.exe"
$portDir = Get-FullPath (Join-Path $vcpkgRoot "ports/ffmpeg")
$installedRoot = Get-FullPath (Join-Path $vcpkgRoot "installed/$Triplet")
$bundleName = "ffmpeg-source-$Version"
$zipPath = Join-Path $outputDir "$bundleName.zip"
$stageDir = Join-Path $outputDir ".ffmpeg-source-stage-$Version"
$workDir = Join-Path $outputDir ".ffmpeg-source-work-$Version"

try {
  if (!(Test-Path -LiteralPath $vcpkgExe -PathType Leaf)) {
    throw "vcpkg executable was not found: $vcpkgExe"
  }
  if (!(Test-Path -LiteralPath $portDir -PathType Container)) {
    throw "FFmpeg vcpkg port directory was not found: $portDir"
  }

  $portFilePath = Join-Path $portDir "portfile.cmake"
  $portManifestPath = Join-Path $portDir "vcpkg.json"
  if (!(Test-Path -LiteralPath $portFilePath -PathType Leaf) -or
      !(Test-Path -LiteralPath $portManifestPath -PathType Leaf)) {
    throw "The FFmpeg portfile or vcpkg manifest is missing from $portDir"
  }

  if (Test-Path -LiteralPath $stageDir) {
    Remove-Item -LiteralPath $stageDir -Recurse -Force
  }
  if (Test-Path -LiteralPath $workDir) {
    Remove-Item -LiteralPath $workDir -Recurse -Force
  }
  if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
  }
  New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
  New-Item -ItemType Directory -Force -Path $workDir | Out-Null

  $portFileText = Get-Content -LiteralPath $portFilePath -Raw
  $portManifest = Get-Content -LiteralPath $portManifestPath -Raw | ConvertFrom-Json
  $portVersion = Get-ManifestVersion $portManifest $portManifestPath
  $portVersionNumber = 0
  if ($portManifest.PSObject.Properties.Name -contains "port-version") {
    $portVersionNumber = [int]$portManifest.'port-version'
  }

  $repo = Get-PortValue $portFileText "REPO"
  $sourceRef = Get-PortValue $portFileText "REF"
  $sourceRef = $sourceRef.Replace('${VERSION}', $portVersion)
  $sourceSha512 = Get-PortValue $portFileText "SHA512"

  $patchBlockMatch = [regex]::Match(
    $portFileText,
    '(?ms)^\s*PATCHES\s*(?<value>.*?)(?=^\s*\))'
  )
  if (!$patchBlockMatch.Success) {
    throw "The FFmpeg portfile does not contain a PATCHES block"
  }
  $patches = @(
    [regex]::Matches(
      $patchBlockMatch.Groups["value"].Value,
      '(?m)^\s*(?<patch>"?[^"\s#]+\.patch"?)(?:\s+#.*)?\s*$'
    ) | ForEach-Object { $_.Groups["patch"].Value.Trim('"') }
  )
  if ($patches.Count -eq 0) {
    throw "The FFmpeg portfile PATCHES block is empty"
  }

  $sourceUrl = "https://github.com/$repo/archive/$sourceRef.tar.gz"
  $archivePath = Join-Path $workDir "ffmpeg-source.tar.gz"
  $extractDir = Join-Path $workDir "extracted"
  Invoke-WebRequest -Uri $sourceUrl -OutFile $archivePath

  $actualSha512 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA512).Hash.ToLowerInvariant()
  if ($actualSha512 -ne $sourceSha512.ToLowerInvariant()) {
    throw "The FFmpeg source archive SHA512 does not match the portfile: expected $sourceSha512, got $actualSha512"
  }

  New-Item -ItemType Directory -Force -Path $extractDir | Out-Null
  & tar.exe -xzf $archivePath -C $extractDir
  if ($LASTEXITCODE -ne 0) {
    throw "The FFmpeg source archive could not be extracted"
  }
  $archiveRoots = @(Get-ChildItem -LiteralPath $extractDir -Directory -Force)
  if ($archiveRoots.Count -ne 1) {
    throw "The FFmpeg source archive did not contain exactly one source directory"
  }
  $sourceRoot = Get-FullPath $archiveRoots[0].FullName

  $previousGitConfigNoSystem = $env:GIT_CONFIG_NOSYSTEM
  $env:GIT_CONFIG_NOSYSTEM = "1"
  try {
    Push-Location $sourceRoot
    try {
      & git init --quiet
      if ($LASTEXITCODE -ne 0) {
        throw "Could not initialize the extracted FFmpeg source tree for patch application"
      }

      foreach ($patch in $patches) {
        $patchPath = Get-FullPath (Join-Path $portDir $patch)
        if (!(Test-Path -LiteralPath $patchPath -PathType Leaf)) {
          throw "The FFmpeg port patch was not found: $patchPath"
        }
        & git -c core.longpaths=true -c core.autocrlf=false -c core.filemode=true --work-tree=. --git-dir=.git apply $patchPath --ignore-whitespace --whitespace=nowarn --verbose
        if ($LASTEXITCODE -ne 0) {
          throw "The FFmpeg port patch could not be applied: $patch"
        }
      }
    }
    finally {
      Pop-Location
    }
  }
  finally {
    if ($null -eq $previousGitConfigNoSystem) {
      Remove-Item Env:GIT_CONFIG_NOSYSTEM -ErrorAction SilentlyContinue
    }
    else {
      $env:GIT_CONFIG_NOSYSTEM = $previousGitConfigNoSystem
    }
  }

  $gitMetadataDir = Join-Path $sourceRoot ".git"
  if (Test-Path -LiteralPath $gitMetadataDir) {
    Remove-Item -LiteralPath $gitMetadataDir -Recurse -Force
  }

  $abiInfoPath = Get-FullPath (Join-Path $vcpkgRoot "installed/$Triplet/share/ffmpeg/vcpkg_abi_info.txt")
  if (!(Test-Path -LiteralPath $abiInfoPath -PathType Leaf)) {
    throw "The vcpkg FFmpeg ABI info file was not found: $abiInfoPath"
  }
  $abiInfoText = Get-Content -LiteralPath $abiInfoPath -Raw
  $featuresMatch = [regex]::Match($abiInfoText, '(?m)^features\s+(?<value>[^\r\n]+)')
  if (!$featuresMatch.Success) {
    throw "The vcpkg FFmpeg ABI info does not contain resolved features"
  }
  $features = $featuresMatch.Groups["value"].Value.Trim()

  $includeDir = Join-Path $installedRoot "include"
  $libDir = Join-Path $installedRoot "lib"
  $binDir = Join-Path $installedRoot "bin"
  $avcodecHeader = Join-Path $includeDir "libavcodec/avcodec.h"
  $avcodecLibrary = Join-Path $libDir "avcodec.lib"
  if (!(Test-Path -LiteralPath $avcodecHeader -PathType Leaf) -or
      !(Test-Path -LiteralPath $avcodecLibrary -PathType Leaf)) {
    throw "The installed FFmpeg avcodec development files were not found"
  }

  $probeSource = Join-Path $workDir "avcodec_configuration_probe.c"
  $probeExe = Join-Path $workDir "avcodec_configuration_probe.exe"
  @'
#include <stdio.h>
#include <libavcodec/avcodec.h>

int main(void) {
    const char *configuration = avcodec_configuration();
    if (configuration == NULL) {
        return 2;
    }
    puts(configuration);
    return 0;
}
'@ | Set-Content -LiteralPath $probeSource -Encoding ascii

  $clCommand = Get-Command cl.exe -ErrorAction SilentlyContinue
  if (!$clCommand) {
    throw "MSVC cl.exe was not found for the FFmpeg configuration probe"
  }
  & $clCommand.Source /nologo "/I$includeDir" $probeSource /link "/LIBPATH:$libDir" avcodec.lib "/OUT:$probeExe"
  if ($LASTEXITCODE -ne 0 -or !(Test-Path -LiteralPath $probeExe -PathType Leaf)) {
    throw "The avcodec configuration probe could not be compiled"
  }

  $previousPath = $env:PATH
  try {
    $env:PATH = "$binDir;$previousPath"
    $configurationLines = @(& $probeExe 2>&1)
    $probeExitCode = $LASTEXITCODE
  }
  finally {
    $env:PATH = $previousPath
  }
  if ($probeExitCode -ne 0) {
    throw "The avcodec configuration probe failed: $($configurationLines -join ' ')"
  }
  $configuration = ($configurationLines -join ' ').Trim()
  if ($configuration -notmatch '(^|\s)--disable-static(?=\s|$)' -or
      $configuration -notmatch '(^|\s)--enable-shared(?=\s|$)') {
    throw "The installed FFmpeg was not verified as dynamic/shared: $configuration"
  }
  if ($configuration -match '(^|\s)--enable-(?:gpl|nonfree)(?=\s|$)') {
    throw "The installed FFmpeg configuration enables GPL or nonfree features: $configuration"
  }

  $avcodecDlls = @(Get-ChildItem -LiteralPath $binDir -Filter "avcodec-*.dll" -File -ErrorAction SilentlyContinue)
  if ($avcodecDlls.Count -ne 1) {
    throw "The installed FFmpeg avcodec DLL was not uniquely identified under $binDir"
  }
  $avcodecDll = $avcodecDlls[0]
  $avcodecSha256 = (Get-FileHash -LiteralPath $avcodecDll.FullName -Algorithm SHA256).Hash

  $gitOutput = @(& git -C $vcpkgRoot rev-parse HEAD 2>$null)
  if ($LASTEXITCODE -ne 0 -or $gitOutput.Count -eq 0) {
    throw "The vcpkg checkout commit could not be determined"
  }
  $vcpkgCommit = ($gitOutput -join "").Trim()
  $vcpkgVersionOutput = @(& $vcpkgExe version 2>&1)
  if ($LASTEXITCODE -ne 0 -or $vcpkgVersionOutput.Count -eq 0) {
    throw "The vcpkg version could not be determined"
  }
  $vcpkgVersion = ($vcpkgVersionOutput | Select-Object -First 1).ToString().Trim()

  $sourceDestination = Join-Path $stageDir "ffmpeg-source"
  $portDestination = Join-Path $stageDir "vcpkg-port"
  New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
  Copy-DirectoryContents $sourceRoot $sourceDestination
  Copy-DirectoryContents $portDir $portDestination

  $buildInfo = @(
    "Public CBZ Viewer release/tag: $Version",
    "vcpkg commit: $vcpkgCommit",
    "vcpkg version: $vcpkgVersion",
    "triplet: $Triplet",
    "FFmpeg port/version: ffmpeg $portVersion (port-version $portVersionNumber)",
    "resolved features (vcpkg ABI info): $features",
    "FFmpeg upstream repo: $repo",
    "FFmpeg upstream ref: $sourceRef",
    "FFmpeg source archive URL: $sourceUrl",
    "FFmpeg source SHA512 (expected): $sourceSha512",
    "FFmpeg source SHA512 (verified): $actualSha512",
    "avcodec DLL filename: $($avcodecDll.Name)",
    "avcodec DLL SHA256: $avcodecSha256",
    "avcodec_configuration result: $configuration",
    "source state: GitHub archive with all listed vcpkg FFmpeg patches applied",
    "vcpkg port contents: complete ports/ffmpeg directory",
    "",
    "Applied vcpkg FFmpeg patches (portfile order):"
  )
  $buildInfo += @($patches | ForEach-Object { "- $_" })
  $buildInfo | Set-Content -LiteralPath (Join-Path $stageDir "BUILD_INFO.txt") -Encoding utf8

  Add-Type -AssemblyName System.IO.Compression.FileSystem
  [System.IO.Compression.ZipFile]::CreateFromDirectory(
    $stageDir,
    $zipPath,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $false
  )
  if (!(Test-Path -LiteralPath $zipPath -PathType Leaf) -or (Get-Item -LiteralPath $zipPath).Length -eq 0) {
    throw "FFmpeg source bundle ZIP was not created: $zipPath"
  }
  Write-Host "FFmpeg source bundle: $zipPath"
}
finally {
  if (Test-Path -LiteralPath $stageDir) {
    Remove-Item -LiteralPath $stageDir -Recurse -Force
  }
  if (Test-Path -LiteralPath $workDir) {
    Remove-Item -LiteralPath $workDir -Recurse -Force
  }
}
