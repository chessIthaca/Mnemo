// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The stamping request-body wrapper (plan 7bc0b473): splits the trace's
//! coarse Connect bucket into `upload_ms` (POST sent → request body fully
//! written) and `server_ms` (upload done → response headers) by wrapping
//! the pre-serialized request body in a stream that stamps the
//! upload-done instant as reqwest polls it.
//!
//! Why a wrapper: reqwest offers no upload-progress hook for a static
//! body — the only observable boundary is the body *stream* itself, polled
//! by the transport as the socket accepts bytes. [`stamping_body`] yields
//! the body in [`BODY_PIECE_BYTES`] pieces and stamps when the LAST piece
//! is polled, so a server that drains the body slowly genuinely delays
//! the stamp (the wait lands in `upload_ms`, not silently in the server
//! bucket).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::provider::trace::LlmRequestLog;

/// Per-request upload/server phase accounting, shared between the stamping
/// body wrapper(s) — one per send attempt, so the reasoning_effort fallback
/// retry accumulates BOTH attempts' windows — and the code that observes
/// response headers.
#[derive(Default)]
pub(crate) struct UploadStamps {
    /// Accumulated upload windows (each attempt's POST-send →
    /// body-fully-written), in ms. `None` until the first attempt's body
    /// was fully written.
    upload_ms: Option<u32>,
    /// Accumulated server-wait windows (each attempt's upload-done →
    /// response headers), in ms. `None` until the first window closed.
    server_ms: Option<u32>,
    /// When the current attempt's body upload finished — the anchor the
    /// next server window is measured from. `None` while the current
    /// attempt's body is still being written.
    upload_done: Option<Instant>,
}

impl UploadStamps {
    /// The current attempt's request body was fully written: accumulate
    /// the upload window (`sent_at` → `now`) and anchor the next server
    /// window at `now`. Returns the accumulated upload ms (for the live
    /// record stamp).
    pub(crate) fn note_body_done(&mut self, sent_at: Instant, now: Instant) -> u32 {
        let window = now.duration_since(sent_at).as_millis() as u32;
        self.upload_ms = Some(self.upload_ms.unwrap_or(0) + window);
        self.upload_done = Some(now);
        self.upload_ms.unwrap_or(0)
    }

    /// Response headers arrived (or the attempt died waiting for them):
    /// close the server window anchored at the last upload-done instant.
    /// Returns the accumulated server ms when a window closed, `None`
    /// when the body never finished uploading (no anchor to measure from
    /// — the window then stays inside `connect_ms`).
    pub(crate) fn note_headers(&mut self, now: Instant) -> Option<u32> {
        let done = self.upload_done.take()?;
        let window = now.duration_since(done).as_millis() as u32;
        self.server_ms = Some(self.server_ms.unwrap_or(0) + window);
        self.server_ms
    }

    /// Stamp whatever upload/server windows closed so far onto the trace
    /// record — the terminal re-stamp at every `connect_ms` stamp site, so
    /// a row that ends abnormally still carries the windows that did
    /// complete (absent windows stay `None` and collapse in the UI).
    pub(crate) fn stamp(&self, log: &LlmRequestLog, id: u64) {
        if let Some(ms) = self.upload_ms {
            log.set_upload_ms(id, ms);
        }
        if let Some(ms) = self.server_ms {
            log.set_server_ms(id, ms);
        }
    }
}

/// The stamping wrapper's piece size: the body is handed to reqwest in
/// pieces of this many bytes so the upload-done instant (stamped when the
/// LAST piece is polled) genuinely tracks the socket write — a server
/// that drains the body slowly delays the final piece's poll, and the
/// wait lands in `upload_ms` instead of silently in the server bucket.
const BODY_PIECE_BYTES: usize = 16 * 1024;

/// Wrap the pre-serialized request body in a stamping stream
/// (`reqwest::Body::wrap_stream`): the body is yielded in
/// [`BODY_PIECE_BYTES`] pieces and the final piece's poll stamps the
/// upload-done instant onto the shared accumulator AND live onto the
/// trace record (`upload_ms`), so the Trace tab can distinguish
/// "uploading" from "waiting for first token" while the call is in
/// flight.
///
/// `sent_at` is the send anchor for THIS attempt — the first send passes
/// `post_sent`; the reasoning_effort fallback retry passes its own
/// instant (`post_sent` is not re-stamped, so the first attempt's anchor
/// would misattribute the retry's upload window).
///
/// The caller must set the `Content-Length` header explicitly: a wrapped
/// stream carries no known length for reqwest, and without the header the
/// request would switch to chunked transfer-encoding — a wire-format
/// change for the gateway. The wrapper yields exactly the given bytes, so
/// the length is exact.
pub(crate) fn stamping_body(
    body: Vec<u8>,
    sent_at: Instant,
    stamps: Arc<Mutex<UploadStamps>>,
    trace: Option<(Arc<LlmRequestLog>, u64)>,
) -> reqwest::Body {
    let pieces: VecDeque<Vec<u8>> = body.chunks(BODY_PIECE_BYTES).map(|c| c.to_vec()).collect();
    let stream = futures::stream::unfold(
        (pieces, sent_at, stamps, trace),
        |(mut pieces, sent_at, stamps, trace)| async move {
            let piece = pieces.pop_front()?;
            if pieces.is_empty() {
                // The last piece is being handed over — the request body
                // is fully written from our side of the socket.
                let now = Instant::now();
                let upload_ms = stamps
                    .lock()
                    .expect("UploadStamps lock poisoned")
                    .note_body_done(sent_at, now);
                if let Some((log, id)) = &trace {
                    log.set_upload_ms(*id, upload_ms);
                }
            }
            Some((
                Ok::<Vec<u8>, std::io::Error>(piece),
                (pieces, sent_at, stamps, trace),
            ))
        },
    );
    reqwest::Body::wrap_stream(stream)
}
