# Rocinante installer for Windows (x64 and Arm64).
#
#   powershell -c "irm https://raw.githubusercontent.com/djynnius/rocinante/main/install.ps1 | iex"
#
# Overrides (set as env vars before running):
#   ROCINANTE_VERSION      release tag to install (default: latest)
#   ROCINANTE_INSTALL_DIR  install directory (default: %LOCALAPPDATA%\Rocinante\bin)
#   ROCINANTE_REPO         github owner/repo (default: djynnius/rocinante)
#   ROCINANTE_INSTALL_BASE full URL base for artifacts (testing/mirrors)
$ErrorActionPreference = "Stop"

$Repo = if ($env:ROCINANTE_REPO) { $env:ROCINANTE_REPO } else { "djynnius/rocinante" }
$Version = if ($env:ROCINANTE_VERSION) { $env:ROCINANTE_VERSION } else { "latest" }
$InstallDir = if ($env:ROCINANTE_INSTALL_DIR) { $env:ROCINANTE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Rocinante\bin" }

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
$IsArm64 = $arch -eq [System.Runtime.InteropServices.Architecture]::Arm64
if (-not $IsArm64 -and $arch -ne [System.Runtime.InteropServices.Architecture]::X64) {
    Write-Error "No prebuilt Windows binary for $arch (x64 and Arm64 only)."
}

$Base = if ($env:ROCINANTE_INSTALL_BASE) {
    $env:ROCINANTE_INSTALL_BASE
} elseif ($Version -eq "latest") {
    "https://github.com/$Repo/releases/latest/download"
} else {
    "https://github.com/$Repo/releases/download/$Version"
}

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("rocinante-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Invoke-WebRequest -Uri "$Base/SHA256SUMS" -OutFile (Join-Path $Tmp "SHA256SUMS") -UseBasicParsing
    $Sums = @(Get-Content (Join-Path $Tmp "SHA256SUMS"))
    $Listed = { param($name) $Sums | Where-Object { $_ -match "\s\*?" + [regex]::Escape($name) + "$" } }

    # Releases before the native Arm64 build have only the x64 one, which
    # Windows 11 runs through its x64 emulation (Windows 10 on Arm emulates
    # only 32-bit x86).
    $Target = "x86_64-pc-windows-msvc"
    if ($IsArm64) {
        if (& $Listed "rocinante-aarch64-pc-windows-msvc.zip") {
            $Target = "aarch64-pc-windows-msvc"
        } elseif ([Environment]::OSVersion.Version.Build -lt 22000) {
            Write-Error "This release has no Arm64 build, and Windows 10 on Arm cannot run the x64 one. Windows 11 on Arm can."
        } else {
            Write-Host "this release has no Arm64 build: installing the x64 one, which Windows 11 runs through its x64 emulation."
        }
    }
    $Archive = "rocinante-$Target.zip"

    Write-Host "downloading $Archive ($Version)…"
    Invoke-WebRequest -Uri "$Base/$Archive" -OutFile (Join-Path $Tmp $Archive) -UseBasicParsing

    $sumsLine = & $Listed $Archive | Select-Object -First 1
    if (-not $sumsLine) { Write-Error "$Archive not listed in SHA256SUMS" }
    $Expected = ($sumsLine -split "\s+")[0].ToLower()
    $Actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp $Archive)).Hash.ToLower()
    if ($Expected -ne $Actual) {
        Write-Error "checksum mismatch for $Archive`n  expected: $Expected`n  actual:   $Actual`nRefusing to install."
    }
    Write-Host "checksum verified."

    Expand-Archive -Path (Join-Path $Tmp $Archive) -DestinationPath $Tmp -Force
    $Binary = Join-Path $Tmp "rocinante.exe"
    if (-not (Test-Path $Binary)) { Write-Error "archive did not contain rocinante.exe" }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Move-Item -Path $Binary -Destination (Join-Path $InstallDir "rocinante.exe") -Force

    $Installed = & (Join-Path $InstallDir "rocinante.exe") --version
    Write-Host "installed: $Installed → $InstallDir\rocinante.exe"

    # Idempotently add to the user PATH.
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (($UserPath -split ";") -notcontains $InstallDir) {
        [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
        Write-Host "added $InstallDir to your user PATH — restart your terminal to pick it up."
    }
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
