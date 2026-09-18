+++
title = "capture shell/build evidence logs as UTF-8 — PowerShell redirect defaults to UTF-16LE"
created = "2027-01-11"
+++

Capturing command output with a PowerShell redirect (`cmd > log.txt`, `Out-File`) on this box writes UTF-16LE. That silently breaks the repo's own tooling: read_files refuses the file ("stream did not contain valid UTF-8"), the content index/search cannot see inside it, and with `.gitattributes` `* text=auto eol=lf` git classifies it as binary — an un-diffable, un-greppable blob that outlives the session. Round-1 reviewer finding LOW 1 on plan 96fe545c (2027-01-16) was exactly this on .coding/analysis/mac-conf-warning.txt and …-after.txt; the fix was an in-place re-encode, justified as the shell fallback (no file tool expresses a BOM/CRLF → UTF-8/LF conversion, and file_write would have meant retyping machine-generated output):

  $p = ".coding\analysis\log.txt"
  $t = [IO.File]::ReadAllText($p)
  [IO.File]::WriteAllText($p, $t, (New-Object Text.UTF8Encoding($false)))

i.e. read the text, write it back with UTF8Encoding($false) (no BOM) — normalizes CRLF→LF too, since ReadAllText collapses CRs into the string model. VERIFY with the repo's tools, not with Select-String (which auto-detects UTF-16 and hides the problem): read_files must return the text, a literal `search` must hit inside it, and a regex `\r` scan must return nothing. Prefer piping to a cmdlet + Select-String only when the log need not outlive the run.
