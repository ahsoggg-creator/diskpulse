# Builds the release artifacts into dist\ and (with -Publish) creates a GitHub release.
#
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1                  # checks + build + dist\
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -SkipChecks      # build + dist\ only
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Publish -NotesFile docs\release-notes\v0.1.0.md
#
# Assets: DiskPulse-Setup.exe (installer: the same exe under another name), DiskPulse.exe (portable; also what the
# auto-updater downloads), diskpulse-cli.exe and SHA256SUMS.txt. When a code-signing certificate "CN=ahsoggg" exists on
# this machine (scripts\make_cert.ps1) the executables are signed before the checksums are computed; CI builds are
# unsigned.
param([switch]$Publish, [switch]$SkipChecks, [string]$NotesFile = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$tag = "v$version"
Write-Host "DiskPulse $tag"

if (-not $SkipChecks) {
    cargo fmt --check
    if ($LASTEXITCODE -ne 0) { throw "cargo fmt --check failed" }
    cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "clippy failed" }
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { throw "tests failed" }
}
cargo build --release -p diskpulse-app -p diskpulse-cli
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$dist = Join-Path $root "dist"
if (Test-Path $dist) { Remove-Item -LiteralPath $dist -Recurse -Force }
New-Item -ItemType Directory $dist | Out-Null
Copy-Item target\release\DiskPulse.exe (Join-Path $dist "DiskPulse.exe")
Copy-Item target\release\diskpulse-cli.exe (Join-Path $dist "diskpulse-cli.exe")

$hasCert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert -ErrorAction SilentlyContinue | Where-Object { $_.Subject -eq "CN=ahsoggg" }
if ($hasCert) {
    & (Join-Path $PSScriptRoot "sign.ps1") (Join-Path $dist "DiskPulse.exe") (Join-Path $dist "diskpulse-cli.exe")
} else {
    Write-Warning "certificate CN=ahsoggg not found - the executables are UNSIGNED (see scripts\make_cert.ps1)"
}
# The installer is the same executable under another name; it is copied after signing so the signature is identical.
Copy-Item (Join-Path $dist "DiskPulse.exe") (Join-Path $dist "DiskPulse-Setup.exe")

$sums = foreach ($f in "DiskPulse-Setup.exe", "DiskPulse.exe", "diskpulse-cli.exe") {
    $h = (Get-FileHash (Join-Path $dist $f) -Algorithm SHA256).Hash.ToLower()
    "$h  $f"
}
Set-Content -LiteralPath (Join-Path $dist "SHA256SUMS.txt") -Value ($sums -join "`n") -Encoding ascii
Get-ChildItem $dist | Format-Table Name, Length

if ($Publish) {
    if ($NotesFile -eq "") { throw "-Publish needs -NotesFile" }
    $assets = Get-ChildItem $dist | ForEach-Object { $_.FullName }
    gh release create $tag @assets --title "DiskPulse $version" --notes-file $NotesFile
    if ($LASTEXITCODE -ne 0) { throw "gh release failed" }
}
