+++
title = "Mnemo2 model price rows are shared by model id, and blank prices still cost"
supersedes = "2026-11-05-mnemo2-model-prices-are-edited-only-on-the-endpo-2"
created = "2026-11-08"
+++

Mnemo2's prices live in `[[pricing]]` rows in the shared endpoints.toml, keyed by model id, so two endpoints with the same id share one price. Mnemo1's Savings settings keep their own table on the same rows. Cost figures still resolve through src-tauri/src/mnemo2/pricing.rs, so a blank price on the Endpoints page can still be costed from the built-in vendor rates, which the page does not show. Where prices are edited is in the sibling record about the Endpoints page.
