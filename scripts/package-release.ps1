param(
    [string]$OutputRoot = 'release_packages'
)

$ErrorActionPreference = 'Stop'
$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
Set-Location $repoRoot

function Resolve-UnderRepo {
    param([string]$PathValue)
    $candidate = if ([System.IO.Path]::IsPathRooted($PathValue)) {
        $PathValue
    } else {
        Join-Path $repoRoot $PathValue
    }
    $full = [System.IO.Path]::GetFullPath($candidate)
    $prefix = $repoRoot.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
    if (-not $full.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Release paths must stay inside the repository: $PathValue"
    }
    return $full
}

function Invoke-Step {
    param(
        [string]$Title,
        [scriptblock]$Action
    )
    Write-Host "==> $Title"
    & $Action
    if ($LASTEXITCODE -ne 0) {
        throw "Step failed ($Title) with exit code $LASTEXITCODE"
    }
}

function Normalize-Architecture {
    $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
    switch ($architecture) {
        'x64' { return 'x86_64' }
        'arm64' { return 'aarch64' }
        default { return $architecture }
    }
}

function Get-SourceDateEpoch {
    if ($env:SOURCE_DATE_EPOCH -and $env:SOURCE_DATE_EPOCH -match '^\d+$') {
        return [Int64]$env:SOURCE_DATE_EPOCH
    }
    $value = (& git log -1 --format=%ct 2>$null)
    if ($LASTEXITCODE -eq 0 -and $value -match '^\d+$') {
        return [Int64]$value
    }
    throw 'SOURCE_DATE_EPOCH is not set and the Git commit time is unavailable.'
}

function New-DeterministicZip {
    param(
        [string]$SourceDirectory,
        [string]$ArchivePath,
        [Int64]$Epoch
    )
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    if (Test-Path -LiteralPath $ArchivePath) {
        Remove-Item -LiteralPath $ArchivePath -Force
    }
    $minimumEpoch = 315532800
    $entryTime = [DateTimeOffset]::FromUnixTimeSeconds([Math]::Max($Epoch, $minimumEpoch))
    $sourceParent = [System.IO.Path]::GetFullPath((Split-Path -Parent $SourceDirectory))
    $sourceParentPrefix = $sourceParent.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
    $rootName = Split-Path -Leaf $SourceDirectory
    $fileStream = [System.IO.File]::Open($ArchivePath, [System.IO.FileMode]::CreateNew)
    try {
        $archive = [System.IO.Compression.ZipArchive]::new(
            $fileStream,
            [System.IO.Compression.ZipArchiveMode]::Create,
            $false
        )
        try {
            $directories = @((Get-Item -LiteralPath $SourceDirectory)) + @(
                Get-ChildItem -LiteralPath $SourceDirectory -Recurse -Directory
            )
            $directories |
                Sort-Object FullName |
                ForEach-Object {
                    $fullName = [System.IO.Path]::GetFullPath($_.FullName)
                    if (-not $fullName.StartsWith($sourceParentPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                        throw "ZIP entry escaped the package root: $fullName"
                    }
                    $relative = $fullName.Substring($sourceParentPrefix.Length).Replace('\', '/') + '/'
                    $entry = $archive.CreateEntry($relative)
                    $entry.LastWriteTime = $entryTime
                }
            Get-ChildItem -LiteralPath $SourceDirectory -Recurse -File |
                Sort-Object FullName |
                ForEach-Object {
                    $fullName = [System.IO.Path]::GetFullPath($_.FullName)
                    if (-not $fullName.StartsWith($sourceParentPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                        throw "ZIP entry escaped the package root: $fullName"
                    }
                    $relative = $fullName.Substring($sourceParentPrefix.Length).Replace('\', '/')
                    $entry = $archive.CreateEntry(
                        $relative,
                        [System.IO.Compression.CompressionLevel]::Optimal
                    )
                    $entry.LastWriteTime = $entryTime
                    $input = [System.IO.File]::OpenRead($_.FullName)
                    try {
                        $output = $entry.Open()
                        try {
                            $input.CopyTo($output)
                        } finally {
                            $output.Dispose()
                        }
                    } finally {
                        $input.Dispose()
                    }
                }
        } finally {
            $archive.Dispose()
        }
    } finally {
        $fileStream.Dispose()
    }
    [System.IO.File]::SetLastWriteTimeUtc($ArchivePath, [DateTimeOffset]::FromUnixTimeSeconds($Epoch).UtcDateTime)
}

$npm = Get-Command npm.cmd -ErrorAction SilentlyContinue
if (-not $npm) {
    $npm = Get-Command npm -ErrorAction SilentlyContinue
}
if (-not $npm) {
    throw 'Node.js and npm are required for a clean release build.'
}

$node = Get-Command node.exe -ErrorAction SilentlyContinue
if (-not $node) {
    $node = Get-Command node -ErrorAction SilentlyContinue
}
if (-not $node) {
    throw 'Node.js is required for release package verification.'
}

$architecture = Normalize-Architecture
$sourceDateEpoch = Get-SourceDateEpoch
$env:SOURCE_DATE_EPOCH = $sourceDateEpoch.ToString()
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_TARGET_DIR = Resolve-UnderRepo "target\package-release\windows-$architecture"
$outputRootPath = Resolve-UnderRepo $OutputRoot
New-Item -ItemType Directory -Force -Path $outputRootPath | Out-Null

Invoke-Step 'Installing locked frontend dependencies' {
    Push-Location (Join-Path $repoRoot 'web-ui')
    try {
        & $npm.Source ci
    } finally {
        Pop-Location
    }
}

Invoke-Step 'Building React assets for embedding' {
    Push-Location (Join-Path $repoRoot 'web-ui')
    try {
        & $npm.Source run build
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath (Join-Path $repoRoot 'web-ui\dist\index.html'))) {
    throw 'Frontend build did not create web-ui\dist\index.html.'
}

Invoke-Step 'Building locked production binaries with browser and DuckDB OLAP' {
    cargo build --locked --release --no-default-features --features release-package --bin BaseSearch --bin base-search-cli
}

$metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) {
    throw 'Could not read Cargo package metadata.'
}
$version = ($metadata.packages | Where-Object { $_.name -eq 'base-search' } | Select-Object -First 1).version
if (-not $version) {
    throw 'Could not determine Base Search version.'
}
$gitSha = (& git rev-parse --short=12 HEAD 2>$null)
if ($LASTEXITCODE -ne 0 -or -not $gitSha) {
    throw 'Could not determine the source revision.'
}

$packageName = "BaseSearch-$version-windows-$architecture"
$packageDir = Join-Path $outputRootPath $packageName
$archivePath = Join-Path $outputRootPath "$packageName.zip"
$checksumPath = "$archivePath.sha256"
if (Test-Path -LiteralPath $packageDir) {
    Remove-Item -LiteralPath $packageDir -Recurse -Force
}
foreach ($path in @($archivePath, $checksumPath)) {
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
    }
}
New-Item -ItemType Directory -Force -Path $packageDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $packageDir 'data') | Out-Null

$releaseBinDir = Join-Path $env:CARGO_TARGET_DIR 'release'
foreach ($binary in @('BaseSearch.exe', 'base-search-cli.exe')) {
    $source = Join-Path $releaseBinDir $binary
    if (-not (Test-Path -LiteralPath $source)) {
        throw "Release binary is missing: $source"
    }
    Copy-Item -LiteralPath $source -Destination (Join-Path $packageDir $binary) -Force
}
Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination (Join-Path $packageDir 'LICENSE') -Force

$launcher = "@echo off`r`ncd /d `"%~dp0`"`r`nstart `"`" `"%~dp0BaseSearch.exe`"`r`n"
[System.IO.File]::WriteAllText(
    (Join-Path $packageDir 'Open Base Search.cmd'),
    $launcher,
    [System.Text.Encoding]::ASCII
)

Invoke-Step 'Rendering release instructions' {
    & $node.Source scripts/release-package.mjs render-readme `
        --template scripts/release/README.txt.in `
        --out (Join-Path $packageDir 'README.txt') `
        --platform windows `
        --arch $architecture `
        --version $version `
        --git-sha $gitSha `
        --epoch $sourceDateEpoch
}

Invoke-Step 'Writing release manifest' {
    & $node.Source scripts/release-package.mjs write-manifest `
        --root $packageDir `
        --platform windows `
        --arch $architecture `
        --version $version `
        --git-sha $gitSha `
        --epoch $sourceDateEpoch
}

Invoke-Step 'Validating release package' {
    & $node.Source scripts/release-package.mjs verify --root $packageDir --platform windows
}

Write-Host '==> Creating deterministic ZIP archive'
New-DeterministicZip -SourceDirectory $packageDir -ArchivePath $archivePath -Epoch $sourceDateEpoch
$archiveHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText(
    $checksumPath,
    "$archiveHash  $([System.IO.Path]::GetFileName($archivePath))`n",
    [System.Text.Encoding]::ASCII
)
[System.IO.File]::SetLastWriteTimeUtc(
    $checksumPath,
    [DateTimeOffset]::FromUnixTimeSeconds($sourceDateEpoch).UtcDateTime
)

Write-Host "Package folder: $packageDir"
Write-Host "Package archive: $archivePath"
Write-Host "SHA-256: $archiveHash"
