// Types for scripts/test-tmp.mjs. The implementation is plain JavaScript so the
// `node --test` script tests, vitest.config.ts and the vitest guard can share it
// without adding Node types to mnemo2's TypeScript program.

/** The checkout's test temp root, `<repo root>/target/tmp`: created if missing and set as TMPDIR, TEMP and TMP on `process.env`. */
export function testTmpRoot(): string;

/** The checkout root holding `moduleUrl` (a `file:` URL), canonicalized with realpath. */
export function checkoutRootOf(moduleUrl: string): string;

/** What `os.tmpdir()` reports right now, canonicalized with realpath. */
export function tmpdirNow(): string;

/** The checkout's test temp folder, `<checkout root>/target/tmp`, worked out from `moduleUrl`. */
export function targetTmpOf(moduleUrl: string): string;

/** Whether two paths name the same folder (case-insensitively on Windows). */
export function samePath(a: string, b: string): boolean;

/** Whether `child` lies strictly inside `parent` (case-insensitively on Windows). */
export function isInside(child: string, parent: string): boolean;
