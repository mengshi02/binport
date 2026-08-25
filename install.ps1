$ErrorActionPreference = "Stop"

$repository = "mengshi02/binport"
$version = if ($env:BINPORT_VERSION) { $env:BINPORT_VERSION } else { "latest" }

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($architecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
    throw "binport: unsupported Windows architecture: $architecture (the Windows client supports x64)"
}

$asset = "binport-windows-amd64.zip"
if ($env:BINPORT_RELEASE_URL) {
    $releaseUrl = $env:BINPORT_RELEASE_URL.TrimEnd('/')
} elseif ($version -eq "latest") {
    $releaseUrl = "https://github.com/$repository/releases/latest/download"
} else {
    $releaseUrl = "https://github.com/$repository/releases/download/$version"
}

$installDir = if ($env:BINPORT_INSTALL_DIR) {
    $env:BINPORT_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "binport\bin"
}

$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("binport-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    Write-Host "binport: downloading $asset"
    Invoke-WebRequest "$releaseUrl/$asset" -OutFile (Join-Path $temporary $asset)
    Invoke-WebRequest "$releaseUrl/SHA256SUMS" -OutFile (Join-Path $temporary "SHA256SUMS")

    $checksumLine = Get-Content (Join-Path $temporary "SHA256SUMS") |
        Where-Object { $_ -match "^[0-9a-fA-F]{64}\s+\*?$([regex]::Escape($asset))$" } |
        Select-Object -First 1
    if (-not $checksumLine) {
        throw "binport: checksum for $asset was not found"
    }
    $expected = ($checksumLine -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash (Join-Path $temporary $asset) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "binport: checksum mismatch for $asset"
    }

    Expand-Archive (Join-Path $temporary $asset) -DestinationPath $temporary
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Copy-Item (Join-Path $temporary "binport-windows-amd64\binport.exe") (Join-Path $installDir "binport.exe") -Force
    Write-Host "binport: installed to $(Join-Path $installDir 'binport.exe')"
    if (($env:PATH -split ';') -notcontains $installDir) {
        Write-Host "binport: add $installDir to your user PATH"
    }
} finally {
    Remove-Item -Recurse -Force $temporary -ErrorAction SilentlyContinue
}
