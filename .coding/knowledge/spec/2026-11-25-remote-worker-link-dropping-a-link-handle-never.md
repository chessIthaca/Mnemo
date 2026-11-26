+++
title = "remote worker link — dropping a Link handle never closes it; use Link::close"
created = "2026-11-25"
evidence = "src/remote/link.rs Link::close; hub.rs sweep_revoked and halt; tests revoked_live_node_is_cut, hub_shutdown_closes_node_links, catalog_change_reaches_node_resolver."
+++

In src/remote (the hub/node link, feature `remote`), a Link fails only when its LAST handle drops, and every connection task holds its own clone while parked on link.closed(). Any place that must cut a node (revocation via deleting <state_dir>/<node_id>.json, HubServer halt, or attach replacing an older link) must call Link::close(), which runs the same LinkInner::fail as a socket error. Related trap: a session pinned to an "endpoint/model" the node's applied catalog does not know silently runs on the default model (Engine::spawn only parses the pin; resolve_turn_provider_routed falls back on None). node.status reports catalog_revision after the resolver has applied it; wait on it before dispatching to a node after a tier change.
