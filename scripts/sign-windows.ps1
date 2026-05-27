# Signs Windows PE files on the self-hosted Windows runner.
#
# This repository is PUBLIC, so NO certificate paths, key identifiers, GCP
# project IDs, or other infrastructure-specific values are hardcoded here.
# Every infrastructure-specific parameter is read from an environment variable;
# the release workflow injects them from repository secrets so the values never
# land in the public source tree or the public build logs (GitHub masks secret
# values in logs). Only generic, non-sensitive values are defaulted (the Windows
# SDK signtool path, the provider name, and the public timestamp URL).
#
# Required environment variables (configure as repository secrets):
#   WINDOWS_SIGN_CERT_PATH      Path to the public signing certificate (.crt) on the runner
#   WINDOWS_SIGN_CA_CERT_PATH   Path to the issuing CA certificate (.crt) on the runner
#   WINDOWS_SIGN_KEY_CONTAINER  Key container/URI passed to the CSP (e.g. a Cloud KMS key resource)
#
# Optional overrides (sensible non-sensitive defaults below):
#   WINDOWS_SIGNTOOL_PATH       signtool.exe location
#   WINDOWS_SIGN_CSP            Cryptographic provider / KSP name
#   WINDOWS_SIGN_TIMESTAMP_URL  RFC 3161 timestamp authority URL

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Path,

    [switch]$CheckOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-OptionalSetting {
    param(
        [Parameter(Mandatory = $true)] [string]$Name,
        [Parameter(Mandatory = $true)] [string]$DefaultValue
    )

    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        return $DefaultValue
    }
    return $value
}

function Get-RequiredSetting {
    param(
        [Parameter(Mandatory = $true)] [string]$Name
    )

    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        throw "Required signing variable '$Name' is not set. Configure it as a repository secret and pass it through the workflow 'env:' block."
    }
    return $value
}

$SignTool          = Get-OptionalSetting "WINDOWS_SIGNTOOL_PATH"      "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe"
$Csp               = Get-OptionalSetting "WINDOWS_SIGN_CSP"           "Google Cloud KMS Provider"
$TimestampUrl      = Get-OptionalSetting "WINDOWS_SIGN_TIMESTAMP_URL" "http://timestamp.sectigo.com"
$CertificatePath   = Get-RequiredSetting "WINDOWS_SIGN_CERT_PATH"
$CaCertificatePath = Get-RequiredSetting "WINDOWS_SIGN_CA_CERT_PATH"
$KeyContainer      = Get-RequiredSetting "WINDOWS_SIGN_KEY_CONTAINER"

foreach ($requiredPath in @($SignTool, $CertificatePath, $CaCertificatePath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "Required Windows signing file not found: $requiredPath"
    }
}

# Intentionally do NOT echo $KeyContainer (it identifies the KMS key/project).
Write-Host "Windows signing provider: $Csp"
Write-Host "Timestamp URL: $TimestampUrl"

if ($CheckOnly) {
    Write-Host "Windows signing prerequisites are present."
    exit 0
}

if ([string]::IsNullOrWhiteSpace($Path)) {
    throw "Target path is required."
}

$TargetPath = $Path.Trim('"')
if (-not (Test-Path -LiteralPath $TargetPath)) {
    throw "Target file not found: $TargetPath"
}

Write-Host "Signing: $TargetPath"

& $SignTool sign `
    /v `
    /fd SHA256 `
    /td SHA256 `
    /tr $TimestampUrl `
    /f $CertificatePath `
    /ac $CaCertificatePath `
    /csp $Csp `
    /kc $KeyContainer `
    $TargetPath

if ($LASTEXITCODE -ne 0) {
    throw "signtool failed with exit code $LASTEXITCODE for: $TargetPath"
}

& $SignTool verify /pa $TargetPath
if ($LASTEXITCODE -ne 0) {
    Write-Host "::warning::signtool verify /pa returned exit $LASTEXITCODE"
}
