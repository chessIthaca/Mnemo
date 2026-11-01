+++
title = "Mnemo2 UI type scale and font families"
created = "2027-01-11"
+++

mnemo2/src/app.css :root owns one type scale: --text-2xs 8px (badges, meta), --text-xs 9px (list summaries, small labels), --text-sm 11px (labels, buttons, names; also the root font-size, so nothing falls back to the browser's 16px), --text-md 13px (prose reading text), --text-lg 16px (compact titles), --text-xl 26px (page titles). Components use only these tokens for font-size; no hard-coded px font sizes outside app.css. Families: --font-prose (Source Serif 4) for long-form reading text (message bodies, plan text, full briefs, markdown); --font-ui (Inter) for chrome (names, labels, buttons, badges, list summaries, meta); --font-mono for code, branches, paths and commits. History: a "UI polish" commit once dropped whole rule blocks from components (WorkerDetail tabs and panels, WorkerRow summary), leaving text at 16px browser defaults. After large style edits, check for markup classes without any rule.
