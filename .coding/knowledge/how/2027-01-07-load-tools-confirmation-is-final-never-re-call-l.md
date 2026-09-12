+++
title = "load_tools confirmation is final — never re-call load_tools, call the target tool directly"
created = "2027-01-07"
status = "superseded"
+++

load_tools(group) success says "its tools join your tool list from your next message on — continue, then call them." The loaded tools' schemas may not be rendered in the visible tool list — that is NOT evidence the load failed. Re-calling load_tools returns "already loaded" and wastes a round-trip (done 4x consecutively in one session, 2027-01-09, image group — even immediately after acknowledging the mistake). Correct behavior: emit the target tool immediately with best-guess args from the system prompt's documentation (e.g. image_analysis: image path + task, detail_level='fine' for small text); a schema-mismatch error teaches the real shape and is strictly cheaper than a redundant load. If emission keeps rerouting to load_tools anyway, abandon the tool path and answer from documented evidence instead — do not burn more turns on the group loader.
