+++
title = "testing-library rerender re-runs every prop-reading effect in Svelte 5"
created = "2026-11-06"
+++

@testing-library/svelte's rerender swaps the whole props object, so any $effect reading any prop re-runs on each rerender even when that prop is unchanged. A test counting calls made by a load effect will see extra calls. Read the prop through a $derived in the component so the effect only re-runs when the value changes.
