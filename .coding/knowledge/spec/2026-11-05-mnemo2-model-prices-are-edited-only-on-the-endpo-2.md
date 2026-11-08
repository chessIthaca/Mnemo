+++
title = "Mnemo2 model prices are edited only on the Endpoints page"
created = "2026-11-05"
status = "superseded"
+++

Mnemo2's settings edit model prices only with each model on the Endpoints page (saved by Setup::upsert_endpoint_detail). The Savings page has levers and knobs only, and model_settings_save refuses a "pricing" section. Rows still live in [[pricing]] in the shared endpoints.toml, keyed by model id, so two endpoints with the same id share one price. Mnemo1's Savings settings keep their own table on the same rows. Cost figures still resolve through src-tauri/src/mnemo2/pricing.rs, so a blank price on Endpoints can still be costed from the built-in vendor rates, which the page does not show.
