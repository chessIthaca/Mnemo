+++
title = "workflow action pins (codeql v4.38.3, upload/download-artifact, setup-node) and how to verify a SHA"
created = "2026-11-05"
+++

The .github/workflows action pins in this repo were moved to these commit SHAs, with the trailing `# vN` version comments unchanged (guard: tests/integration/ci_workflow.rs::workflows_pin_actions_to_full_shas requires a full 40-hex SHA on every `uses:` line; dtolnay/rust-toolchain@stable is the only ALLOWED_UNPINNED exception):

- github/codeql-action/init|analyze: 24c54180a607b1449ed407dd24f251e4e9147c8d (v4.38.3; previous pin 2892aa5e19bbd11bc0cff5427e3b750a04d9e3c2)
- actions/upload-artifact: cf430e030ddbb5b0abf93d22962f4752f3646cd9 (v7.0.2; previous 043fb46d1a93c77aae656e7c1c64a875d1fc6a0a)
- actions/download-artifact: 9000827ccba6bdab643e8b6fd33ac0654aef8333 (v8.0.2; previous 3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c)
- actions/setup-node: 949feb2413d6458794dcd2491c4babbbce0c15c1 (v7.1.0; previous 820762786026740c76f36085b0efc47a31fe5020)

Unchanged in the same files: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 (v7), Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 (v2), softprops/action-gh-release@efb35369e0ad2afab669f228072c1b0d510eae64 (v3), dtolnay/rust-toolchain@stable.

How to verify a pin before trusting it: GET https://api.github.com/repos/<owner>/<repo>/git/ref/tags/<vN> returns object.sha; when object.type is "tag" it is an annotated tag object, so dereference it via /git/tags/<sha> to reach the commit. Caveat learned the hard way: the moving major tag (e.g. codeql-action v4) can point PAST the release a Dependabot bump names, so confirm the specific release commit instead — for 24c54180 the commit message is "Merge pull request #4214 from github/update-v4.38.3-..." and its first parent is the previous pin, which proves the bump is one genuine release step rather than a rewind. The `uses: owner/repo/path@sha` ref must always be a COMMIT sha, never the annotated tag object's sha.
