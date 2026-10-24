# Downloads Bun's prebuilt JavaScriptCore (oven-sh/WebKit) into vendor/bun-webkit.
param(
    [string]$Tag = "autobuild-680ef47cec2ce646fc109de005882a17542c700d",
    [string]$Arch = $(if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" })
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$vendor = Join-Path $root "vendor"
$archive = Join-Path $vendor "bun-webkit.tar.gz"
$url = "https://github.com/oven-sh/WebKit/releases/download/$Tag/bun-webkit-windows-$Arch.tar.gz"

New-Item -ItemType Directory -Force $vendor | Out-Null
Write-Host "Downloading $url"
curl.exe -L --fail -o $archive $url
if ($LASTEXITCODE -ne 0) { throw "Download failed" }

Write-Host "Extracting to $vendor\bun-webkit"
tar -xzf $archive -C $vendor
if ($LASTEXITCODE -ne 0) { throw "Extraction failed" }
Remove-Item $archive
Write-Host "Done."
