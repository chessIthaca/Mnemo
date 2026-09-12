# Copyright (c) 2026 Carsten Hess
# SPDX-License-Identifier: MIT
# See LICENSE in the repository root.

<#
.SYNOPSIS
  Prepends the Mnemo copyright + license header to every source file.

.DESCRIPTION
  Covers: src/**/*.rs, src-tauri/**/*.rs, tests/**/*.rs,
  frontend/src/**/*.ts + *.tsx, frontend/*.ts (config files).
  Skips .coding/, target/, node_modules/, dist/ (not scanned) and any file
  whose first 500 chars already contain the copyright marker (idempotent).

  Byte-level read/write keeps encoding intact. A leading UTF-8 BOM decodes to
  a U+FEFF char inside the string; it is detected, held aside, and re-emitted
  at byte 0 (BEFORE the header) because rustc only tolerates a BOM at the very
  start of a file. Each file's own line-ending style (CRLF vs LF) is used in
  the inserted header. `//` line comments are legal at the top of Rust files
  even before `#![deny(warnings)]` / `//!` inner doc comments.

.EXAMPLE
  Run from the repository root:
      pwsh scripts/add-copyright-headers.ps1
  Dry run (print what would change, write nothing):
      pwsh scripts/add-copyright-headers.ps1 -WhatIf
#>
[CmdletBinding()]
param(
    [switch]$WhatIf
)

$ErrorActionPreference = 'Stop'
$root   = Split-Path -Parent $PSScriptRoot   # repo root = parent of scripts/
$marker = 'Copyright (c) 2026'

$headerLines = @(
    "// $marker Carsten Hess",
    '// SPDX-License-Identifier: MIT',
    '// See LICENSE in the repository root.',
    ''
)

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
    # it at the very front of the output (BEFORE the inserted header).
    $hasBom = $text.Length -gt 0 -and $text[0] -eq [char]0xFEFF
    if ($hasBom) { $text = $text.Substring(1) }
    $head  = $text.Substring(0, [Math]::Min(500, $text.Length))
    if ($head.Contains($marker)) { $skipped++; continue }
    $eol    = if ($text.Contains("`r`n")) { "`r`n" } else { "`n" }
    $header = ($headerLines -join $eol) + $eol
    if (-not $WhatIf) {
        $payload = $utf8.GetBytes($header + $text)
        if ($hasBom) { $payload = $bomBytes + $payload }
        [System.IO.File]::WriteAllBytes($f.FullName, $payload)
    }
    $edited++
    Write-Output "headered: $($f.FullName.Substring($root.Length + 1))"
}
Write-Output "done: $edited headered, $skipped already headered"
