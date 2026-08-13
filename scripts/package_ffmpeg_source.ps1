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

function Get-LastLogValue {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Text,

    [Parameter(Mandatory = $true)]
    [string]$Pattern
  )

  $matches = [regex]::Matches($Text, $Pattern)
  if ($matches.Count -eq 0) {
    return $null
  }
  return $matches[$matches.Count - 1].Groups["value"].Value.Trim()
}

$vcpkgRoot = Get-FullPath $VcpkgRoot
$outputDir = Get-FullPath $OutputDir
$vcpkgExe = Join-Path $vcpkgRoot "vcpkg.exe"
$buildtreesRoot = Get-FullPath (Join-Path $vcpkgRoot "buildtrees/ffmpeg")
$buildtreeSourceRoot = Get-FullPath (Join-Path $buildtreesRoot "src")
$portDir = Get-FullPath (Join-Path $vcpkgRoot "ports/ffmpeg")
$bundleName = "ffmpeg-source-$Version"
$zipPath = Join-Path $outputDir "$bundleName.zip"
$stageDir = Join-Path $outputDir ".ffmpeg-source-stage-$Version"

try {
  if (!(Test-Path -LiteralPath $vcpkgExe -PathType Leaf)) {
    throw "vcpkg executable was not found: $vcpkgExe"
  }
  if (!(Test-Path -LiteralPath $buildtreesRoot -PathType Container)) {
    throw "FFmpeg buildtrees directory was not found: $buildtreesRoot"
  }
  if (!(Test-Path -LiteralPath $portDir -PathType Container)) {
    throw "FFmpeg vcpkg port directory was not found: $portDir"
  }

  if (Test-Path -LiteralPath $stageDir) {
    Remove-Item -LiteralPath $stageDir -Recurse -Force
  }
  if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
  }
  New-Item -ItemType Directory -Force -Path $outputDir | Out-Null

  $logFiles = @(Get-ChildItem -LiteralPath $buildtreesRoot -Recurse -File -Filter "*.log")
  if ($logFiles.Count -eq 0) {
    throw "No FFmpeg vcpkg build logs were found under $buildtreesRoot"
  }
  $logText = [string]::Join(
    [Environment]::NewLine,
    @($logFiles | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw })
  )

  $sourceMatches = [regex]::Matches($logText, "(?m)^-- Using source at (?<path>[^\r\n]+)")
  if ($sourceMatches.Count -eq 0) {
    throw "The vcpkg FFmpeg logs did not identify the source directory"
  }
  $sourcePathText = $sourceMatches[$sourceMatches.Count - 1].Groups["path"].Value.Trim()
  $sourcePath = Get-FullPath ($sourcePathText.Replace('/', '\'))
  $sourceRootPrefix = $buildtreeSourceRoot.TrimEnd('\') + '\'
  if (!(Test-Path -LiteralPath $sourcePath -PathType Container) -or
      !$sourcePath.StartsWith($sourceRootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "The vcpkg FFmpeg source path is not a source directory under $buildtreeSourceRoot`: $sourcePath"
  }

  $portFilePath = Join-Path $portDir "portfile.cmake"
  $portManifestPath = Join-Path $portDir "vcpkg.json"
  if (!(Test-Path -LiteralPath $portFilePath -PathType Leaf) -or
      !(Test-Path -LiteralPath $portManifestPath -PathType Leaf)) {
    throw "The FFmpeg portfile or vcpkg manifest is missing from $portDir"
  }

  $portFileText = Get-Content -LiteralPath $portFilePath -Raw
  $portManifest = Get-Content -LiteralPath $portManifestPath -Raw | ConvertFrom-Json
  $portVersion = [string]$portManifest.version
  if ([string]::IsNullOrWhiteSpace($portVersion)) {
    throw "The FFmpeg port version is missing from $portManifestPath"
  }
  $portVersionSuffix = ""
  if ($portManifest.PSObject.Properties.Name -contains "port-version") {
    $portVersionSuffix = " (port-version $($portManifest.'port-version'))"
  }

  $repoMatch = [regex]::Match($portFileText, "(?m)^\s*REPO\s+(?<value>[^\s\)]+)")
  $refMatch = [regex]::Match($portFileText, '(?m)^\s*REF\s+"(?<value>[^"]+)"')
  $sha512Match = [regex]::Match($portFileText, "(?m)^\s*SHA512\s+(?<value>[0-9a-fA-F]+)")
  if (!$repoMatch.Success -or !$refMatch.Success -or !$sha512Match.Success) {
    throw "The FFmpeg portfile does not contain complete upstream source identity metadata"
  }
  $sourceRef = $refMatch.Groups["value"].Value.Replace('${VERSION}', $portVersion)
  $sourceSha512 = $sha512Match.Groups["value"].Value

  $portPatchMatches = [regex]::Matches(
    $portFileText,
    "(?m)^\s+(?<patch>\d[^ \t\r\n]+\.patch)(?:\s+#.*)?\s*$"
  )
  $portPatches = @($portPatchMatches | ForEach-Object { $_.Groups["patch"].Value } | Select-Object -Unique)
  if ($portPatches.Count -eq 0) {
    throw "No FFmpeg port patches were found in $portFilePath"
  }

  $appliedPatchMatches = [regex]::Matches($logText, "-- Applying patch (?<patch>[^\r\n]+)")
  $appliedPatches = @($appliedPatchMatches | ForEach-Object { $_.Groups["patch"].Value.Trim() } | Select-Object -Unique)
  $missingPatches = @($portPatches | Where-Object { $_ -notin $appliedPatches })
  if ($missingPatches.Count -gt 0) {
    throw "The vcpkg logs do not confirm application of FFmpeg port patches: $($missingPatches -join ', ')"
  }

  $abiInfoCandidates = @(
    (Join-Path $buildtreesRoot "$Triplet.vcpkg_abi_info.txt"),
    (Join-Path $vcpkgRoot "installed/$Triplet/share/ffmpeg/vcpkg_abi_info.txt")
  )
  $abiInfoPath = $abiInfoCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
  if (!$abiInfoPath) {
    throw "The vcpkg FFmpeg ABI info file was not found"
  }
  $abiInfoText = Get-Content -LiteralPath $abiInfoPath -Raw
  $featuresMatch = [regex]::Match($abiInfoText, "(?m)^features (?<value>[^\r\n]+)")
  if (!$featuresMatch.Success) {
    throw "The vcpkg FFmpeg ABI info does not contain the resolved feature set"
  }
  $features = $featuresMatch.Groups["value"].Value.Trim()

  $commonOptions = Get-LastLogValue $logText "(?m)^-- Building Options: (?<value>[^\r\n]+)"
  $releaseOptions = Get-LastLogValue $logText "(?m)^-- Building Release Options: (?<value>[^\r\n]+)"
  if ([string]::IsNullOrWhiteSpace($commonOptions) -or [string]::IsNullOrWhiteSpace($releaseOptions)) {
    throw "The vcpkg FFmpeg logs do not contain the common and release configure options"
  }
  if ($commonOptions -notmatch "--disable-static" -or $commonOptions -notmatch "--enable-shared") {
    throw "The vcpkg FFmpeg build was not verified as dynamic/shared"
  }

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

  $sourceRelativePath = [System.IO.Path]::GetRelativePath($buildtreesRoot, $sourcePath).Replace('\', '/')
  $sourceIdentity = Split-Path -Leaf $sourcePath
  $sourceDestination = Join-Path $stageDir "ffmpeg-source"
  $portDestination = Join-Path $stageDir "vcpkg-port"
  New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
  Copy-DirectoryContents $sourcePath $sourceDestination
  Copy-DirectoryContents $portDir $portDestination

  $buildInfo = @(
    "CBZ Viewer release/tag: $Version",
    "vcpkg commit/baseline: $vcpkgCommit (C:/vcpkg checkout HEAD; no separate project manifest baseline)",
    "vcpkg version: $vcpkgVersion",
    "triplet: $Triplet",
    "FFmpeg port/version: ffmpeg $portVersion$portVersionSuffix",
    "FFmpeg features (vcpkg ABI info): $features",
    "dynamic/static configuration: dynamic/shared (verified by --disable-static --enable-shared)",
    "FFmpeg upstream repository: $($repoMatch.Groups['value'].Value)",
    "FFmpeg upstream ref: $sourceRef",
    "FFmpeg source archive SHA512 (vcpkg port): $sourceSha512",
    "source directory or source identity: $sourceRelativePath ($sourceIdentity)",
    "source state: vcpkg buildtree source after the portfile PATCHES were applied",
    "source bundle policy: only the source directory is copied; vcpkg buildtrees logs/build outputs are not bundled",
    "vcpkg port contents: complete ports/ffmpeg directory, including portfile, vcpkg.json, templates, usage, and patch files",
    "FFmpeg configure options (common): $commonOptions",
    "FFmpeg configure options (release): $releaseOptions",
    "",
    "Applied vcpkg FFmpeg patches (verified in build logs):"
  )
  $buildInfo += $appliedPatches
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
}
