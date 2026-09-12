# Copyright (c) 2026 Carsten Hess
# SPDX-License-Identifier: MIT
# See LICENSE in the repository root.

<#
.SYNOPSIS
  One-time script: replaces the PolyForm Noncommercial SPDX identifier and
  drops the "(non-commercial use only)" clause in every source file's
  copyright header, relicensing the headers to MIT.

.DESCRIPTION
  Targets the same file set as add-copyright-headers.ps1:
  src/**/*.rs, src-tauri/**/*.rs, tests/**/*.rs,
  frontend/src/**/*.ts + *.tsx, frontend/*.ts (config files), frontend/*.js.
  Skips target/, node_modules/, dist/ (not scanned).

  Two within-line string replacements per file:
    LicenseRef-PolyForm-Noncommercial-1.0.0  ->  MIT
    See LICENSE in the repository root (non-commercial use only).  ->  See LICENSE in the repository root.

  Byte-level read/write keeps encoding intact. A leading UTF-8 BOM decodes to
  a U+FEFF char inside the string; it is detected, held aside, and re-emitted
  at byte 0 (BEFORE the content) because rustc only tolerates a BOM at the very
  start of a file. Line-ending style is preserved because the replacements are
  within-line (no EOL characters touched).

.EXAMPLE
  Run from the repository root:
      pwsh scripts/relicense-to-mit.ps1
  Dry run (print what would change, write nothing):
      pwsh scripts/relicense-to-mit.ps1 -WhatIf
#>
[CmdletBinding()]
param(
    [switch]$WhatIf
)

$ErrorActionPreference = 'Stop'
$root   = Split-Path -Parent $PSScriptRoot   # repo root = parent of scripts/

$targets =
    @(Get-ChildItem -Recurse -Filter *.rs  -Path (Join-Path $root 'src')) +
    @(Get-ChildItem -Recurse -Filter *.rs  -Path (Join-Path $root 'src-tauri')) +
    @(Get-ChildItem -Recurse -Filter *.rs  -Path (Join-Path $root 'tests')) +
    @(Get-ChildItem -Recurse -Filter *.ts  -Path (Join-Path $root 'frontend\src')) +
    @(Get-ChildItem -Recurse -Filter *.tsx -Path (Join-Path $root 'frontend\src')) +
    @(Get-ChildItem -Filter *.ts -Path (Join-Path $root 'frontend')) +
    @(Get-ChildItem -Filter *.js -Path (Join-Path $root 'frontend'))

$utf8 = New-Object System.Text.UTF8Encoding($false)
$bomBytes = [byte[]](0xEF, 0xBB, 0xBF)
$edited = 0
$skipped = 0
foreach ($f in $targets) {
    if ($f.FullName -match '\\(target|node_modules|dist)\\') { continue }
    $bytes = [System.IO.File]::ReadAllBytes($f.FullName)
    $text  = [System.Text.Encoding]::UTF8.GetString($bytes)
    # UTF8.GetString decodes a leading BOM into a U+FEFF char. rustc only
    # tolerates a BOM at byte 0, so strip it from the string here and re-emit
    # it at the very front of the output.
    $hasBom = $text.Length -gt 0 -and $text[0] -eq [char]0xFEFF
    if ($hasBom) { $text = $text.Substring(1) }
    $original = $text
    $text = $text.Replace('LicenseRef-PolyForm-Noncommercial-1.0.0', 'MIT')
    $text = $text.Replace('See LICENSE in the repository root (non-commercial use only).', 'See LICENSE in the repository root.')
    if ($text -eq $original) { $skipped++; continue }
    if (-not $WhatIf) {
        $payload = $utf8.GetBytes($text)
        if ($hasBom) { $payload = $bomBytes + $payload }
        [System.IO.File]::WriteAllBytes($f.FullName, $payload)
    }
    $edited++
    Write-Output "relicensed: $($f.FullName.Substring($root.Length + 1))"
}
Write-Output "done: $edited relicensed, $skipped already MIT"
