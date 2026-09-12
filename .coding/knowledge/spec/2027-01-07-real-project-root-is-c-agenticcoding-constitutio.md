+++
title = "Real project root is C:\\AgenticCoding (constitution's C:\\AgenticCoder\\AgenticCoder is stale)"
created = "2027-01-07"
+++

The ACTUAL project root on this machine is `C:\AgenticCoding` — NOT `C:\AgenticCoder\AgenticCoder` as stated in the project constitution's Environment section (that path does not exist; the constitution is stale on this point). Verified 2027-01-07 via shell `Get-Location` (cwd "." resolves to C:\AgenticCoding) and `Test-Path 'C:\AgenticCoder\AgenticCoder'` → False. The reviewer report path also confirmed it (\\?\C:\AgenticCoding\.coding\reviews\...). Practical rule: in `shell` calls use RELATIVE paths (cwd "." = project root) or absolute paths under C:\AgenticCoding; never trust the constitution's C:\AgenticCoder\AgenticCoder path for file writes.
