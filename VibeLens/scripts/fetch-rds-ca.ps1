# Windows equivalent of fetch-rds-ca.sh
$ErrorActionPreference = "Stop"
$dest = Join-Path $PSScriptRoot "..\src-tauri\resources\rds-global-bundle.pem"
Invoke-WebRequest -Uri "https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem" -OutFile $dest
if (-not (Select-String -Path $dest -Pattern "BEGIN CERTIFICATE" -Quiet)) { throw "download did not look like a PEM bundle" }
Write-Host "RDS CA bundle saved to $dest"
