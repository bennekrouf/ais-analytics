<#
.SYNOPSIS
  One-time setup for ais-analytics on Windows.

.DESCRIPTION
  Installs the Azure CLI, which ais-analytics depends on for authentication —
  it has no sign-in of its own and reads the `az login` session for both the
  ARM calls and the Log Analytics query API.

  Already-installed tools are skipped. Run from an elevated prompt, or let
  the installer invoke it (the installer already runs elevated).

.PARAMETER NoPrompt
  Skip the "press any key" pause at the end. Used by the installer.
#>

param([switch]$NoPrompt)

$ErrorActionPreference = 'Continue'

function Write-Info { param($m) Write-Host "[info]  $m" -ForegroundColor Cyan }
function Write-Ok   { param($m) Write-Host "[ok]    $m" -ForegroundColor Green }
function Write-Skip { param($m) Write-Host "[skip]  $m" -ForegroundColor Yellow }
function Write-Warn { param($m) Write-Host "[warn]  $m" -ForegroundColor Yellow }

function Test-Command {
    param($Name)
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

Write-Host ""
Write-Host "ais-analytics — Windows setup" -ForegroundColor White
Write-Host ""

# ── Azure CLI ────────────────────────────────────────────────────────────────
if (Test-Command 'az') {
    Write-Skip "Azure CLI already installed"
} else {
    Write-Info "Installing Azure CLI (this takes a few minutes)..."
    try {
        # winget is present on Windows 10 1809+ / Server 2022; the MSI is the
        # fallback for images that ship without it.
        if (Test-Command 'winget') {
            winget install --exact --id Microsoft.AzureCLI `
                --accept-package-agreements --accept-source-agreements `
                --silent | Out-Null
        } else {
            $msi = Join-Path $env:TEMP 'azure-cli.msi'
            Invoke-WebRequest -Uri 'https://aka.ms/installazurecliwindowsx64' `
                -OutFile $msi -UseBasicParsing
            Start-Process msiexec.exe -Wait -ArgumentList "/i `"$msi`" /quiet"
            Remove-Item $msi -ErrorAction SilentlyContinue
        }

        # The installer extends the machine PATH, but this process inherited
        # the old one — refresh so the verification below can find `az`.
        $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
                    [Environment]::GetEnvironmentVariable('Path', 'User')

        if (Test-Command 'az') {
            Write-Ok "Azure CLI installed"
        } else {
            Write-Warn "Azure CLI installed but not yet on PATH — restart your terminal."
        }
    } catch {
        Write-Warn "Azure CLI install failed: $_"
        Write-Warn "Install it manually from https://aka.ms/installazurecliwindows"
    }
}

Write-Host ""
Write-Host "Setup complete. Run 'az login', then launch AIS Analytics." -ForegroundColor Green
Write-Host ""

if (-not $NoPrompt) {
    Write-Host "Press any key to close..."
    $null = $Host.UI.RawUI.ReadKey('NoEcho,IncludeKeyDown')
}
