+++
title = "environment date surfaces disagree — tool stamps lag session-recorded dates"
created = "2027-01-11"
+++

The environment's date surfaces disagree: memory_amend/memory_write stamps use the tool clock (e.g. "Amended 2027-01-11"), review report filenames use another (e.g. "2026-09-20-…"), and session-recorded event dates run ahead (e.g. "user request 2027-02-05"). Verified systemic 2027-02-05 (plan 402cc091 review LOW 2: an amendment stamped 2027-01-11 recording a request dated 2027-02-05; the pre-existing 2027-01-07 chat-links spec amendment shows the same skew). Treat tool-stamped dates as the mechanical record stamp and session-recorded dates as the event truth; when both appear on one line, note the skew (as the MCP spec amendment now does).
