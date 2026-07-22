param(
    [Parameter(Mandatory = $true)]
    [string]$PackageDir,
    [int]$TimeoutSeconds = 120
)

$ErrorActionPreference = 'Stop'
$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$packagePath = [System.IO.Path]::GetFullPath($PackageDir)
if (-not (Test-Path -LiteralPath $packagePath -PathType Container)) {
    throw "Package folder is missing: $packagePath"
}

$requireSigning = $env:BASE_SEARCH_REQUIRE_SIGNING -eq '1'
$requireSigningArg = $requireSigning.ToString().ToLowerInvariant()
& node (Join-Path $PSScriptRoot 'release-package.mjs') verify `
    --root $packagePath `
    --platform windows `
    --require-signed $requireSigningArg
if ($LASTEXITCODE -ne 0) {
    throw 'Package layout verification failed.'
}

$manifest = Get-Content -LiteralPath (Join-Path $packagePath 'release-manifest.json') -Raw |
    ConvertFrom-Json
if ($manifest.signing.windows_authenticode -eq 'signed') {
    foreach ($binary in @('BaseSearch.exe', 'base-search-cli.exe')) {
        $binaryPath = Join-Path $packagePath $binary
        $signature = Get-AuthenticodeSignature -FilePath $binaryPath
        if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
            throw "Authenticode verification failed for $binary ($($signature.Status)): $($signature.StatusMessage)"
        }
    }
}

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
$listener.Stop()

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("base-search-package-smoke-" + [Guid]::NewGuid().ToString('N'))
$tempRoot = [System.IO.Path]::GetFullPath($tempRoot)
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
$databasePath = Join-Path $tempRoot 'smoke.db'
$stdoutPath = Join-Path $tempRoot 'server.stdout.log'
$stderrPath = Join-Path $tempRoot 'server.stderr.log'
$launcher = Join-Path $packagePath 'BaseSearch.exe'
$cli = Join-Path $packagePath 'base-search-cli.exe'
$process = $null

try {
    $process = Start-Process `
        -FilePath $launcher `
        -ArgumentList @('--browser', '--db', $databasePath, '--host', '127.0.0.1', '--port', $port, '--no-open') `
        -PassThru `
        -WindowStyle Hidden `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $health = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($process.HasExited) {
            $stdout = if (Test-Path $stdoutPath) { Get-Content -Raw $stdoutPath } else { '' }
            $stderr = if (Test-Path $stderrPath) { Get-Content -Raw $stderrPath } else { '' }
            throw "Packaged server exited before readiness.`nSTDOUT:`n$stdout`nSTDERR:`n$stderr"
        }
        try {
            $health = Invoke-RestMethod -Uri "http://127.0.0.1:$port/api/v2/health" -TimeoutSec 2
            if ($health.status -eq 'ok') {
                break
            }
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $health -or $health.status -ne 'ok') {
        throw "Packaged server did not become ready within $TimeoutSeconds seconds."
    }

    $workspace = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$port/" -TimeoutSec 10
    if ($workspace.StatusCode -ne 200 -or $workspace.Content -notmatch '<div id="root"') {
        throw 'Packaged server did not serve the embedded React application.'
    }
    $engines = Invoke-RestMethod -Uri "http://127.0.0.1:$port/api/v2/engines" -TimeoutSec 30
    if ($engines.duckdb_available -ne $true) {
        throw 'Packaged /api/v2/engines reports that DuckDB is unavailable.'
    }
    Write-Host 'Packaged HTTP smoke passed: React assets and DuckDB capability are present.'
} finally {
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit()
    }
}

& $cli stats $databasePath
if ($LASTEXITCODE -ne 0) {
    throw 'Packaged CLI stats command failed.'
}
& $cli olap-build $databasePath
if ($LASTEXITCODE -ne 0) {
    throw 'Packaged CLI could not build a DuckDB OLAP projection.'
}

$tempPrefix = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
if (-not $tempRoot.StartsWith($tempPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to remove unexpected smoke path: $tempRoot"
}
Remove-Item -LiteralPath $tempRoot -Recurse -Force
Write-Host "Windows package smoke passed: $packagePath"
