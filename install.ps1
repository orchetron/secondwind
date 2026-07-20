# secondwind installer for Windows.
#
#   irm https://raw.githubusercontent.com/OWNER/secondwind/main/install.ps1 | iex
#
# Downloads a checksum-verified binary for this machine. Overrides via env:
#   SECONDWIND_REPO, SECONDWIND_VERSION, INSTALL_DIR
$ErrorActionPreference = "Stop"

$repo = if ($env:SECONDWIND_REPO) { $env:SECONDWIND_REPO } else { "OWNER/secondwind" }
$version = if ($env:SECONDWIND_VERSION) { $env:SECONDWIND_VERSION } else { "latest" }
$dir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { "$env:LOCALAPPDATA\secondwind\bin" }

if (-not [Environment]::Is64BitOperatingSystem) { throw "secondwind: only 64-bit Windows is supported" }
$target = "x86_64-pc-windows-msvc"
$zip = "secondwind-$target.zip"
$base = if ($version -eq "latest") {
  "https://github.com/$repo/releases/latest/download"
} else {
  "https://github.com/$repo/releases/download/$version"
}

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ([guid]::NewGuid()))
$archive = Join-Path $tmp $zip
Invoke-WebRequest "$base/$zip" -OutFile $archive

try {
  $sums = Join-Path $tmp "SHA256SUMS"
  Invoke-WebRequest "$base/SHA256SUMS" -OutFile $sums
  $line = Get-Content $sums | Where-Object { $_ -match [regex]::Escape($zip) } | Select-Object -First 1
  if ($line) {
    $want = ($line -split '\s+')[0].ToLower()
    $got = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $want) { throw "secondwind: checksum mismatch" }
  }
} catch {
  if ($_.Exception.Message -like "*checksum*") { throw }
}

New-Item -ItemType Directory -Force -Path $dir | Out-Null
Expand-Archive -Path $archive -DestinationPath $dir -Force
Write-Host "installed secondwind to $dir"

if (($env:PATH -split ';') -notcontains $dir) {
  Write-Host "add it to your PATH:"
  Write-Host "  setx PATH `"$dir;`$env:PATH`""
}
Write-Host ""
Write-Host "next:  secondwind check    then    secondwind proof"
