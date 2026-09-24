// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The optional Laya classifier — fast, calibrated "System 1" decisions.
//!
//! [`Classifier`] answers typed questions (`choice` / `score` / `noul`)
//! about a state text. The only configured backend is a `laya-serve`
//! instance reached over HTTP (`POST /v1/systemone` — Laya's Jev-compatible
//! wire protocol). The classifier is **opt-in and disabled by default**:
//! with Laya off (or enabled without an endpoint) `build_classifier` returns
//! `None` and the runtime slot stays empty — no backend object exists at all,
//! so there are zero calls, zero cost, and zero new failure modes.
//! [`NoClassifier`] remains available as an explicit no-op (tests, or a
//! caller that wants a total `Classifier` with no endpoint).
//!
//! Like the [`Embedder`](super::Embedder) contract, [`Classifier::classify`]
//! is infallible by design: a disabled backend, an unreachable endpoint, or a
//! malformed response yields `None` (no answer) rather than an error, so
//! callers keep their pre-classifier behavior. Answers are confidence-gated
//! by the caller (see [`Answer::confidence`]) — the shipped checkpoints are
//! near-chance zero-shot on custom tasks and over-confident until a
//! per-(question type, option count) temperature refit on held-out data, so
//! no decision may gate on an unvalidated probability.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A typed question for the classifier — one of Laya's three decision
/// primitives. Serializes to one `questions` entry of the `/v1/systemone`
/// payload (e.g. `{"type":"choice","instructions":"…","criteria":{…}}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Pick one label. `criteria` maps each option label → the description
    /// the model reads. Labels are rendered verbatim — avoid boolean-ish
    /// labels (`true` / `yes`), which the checkpoints can follow instead of
    /// the descriptions.
    Choice {
        /// The question shown to the model.
        instructions: String,
        /// Option label → description, rendered verbatim.
        criteria: BTreeMap<String, String>,
    },
    /// Pick an ordinal level. `criteria` lists the rubric levels from lowest
    /// to highest; the answer is the expected level on that scale.
    Score {
        /// The question shown to the model.
        instructions: String,
        /// Rubric levels, lowest → highest.
        criteria: Vec<String>,
    },
    /// A yes/no probability (Laya's `noul` primitive): the answer is the
    /// calibrated `P(true)` in 0.0–1.0.
    NoUl {
        /// The question shown to the model.
        instructions: String,
        /// Optional overrides for the model-facing option text. When present,
        /// Laya requires exactly the keys `true` and `false`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<BTreeMap<String, String>>,
    },
}

/// A classifier's answer to one [`Question`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Answer {
    /// A choice answer: the top label + the calibrated confidence of that
    /// pick; `probabilities` carries the per-option distribution when the
    /// backend reports one (empty otherwise).
    Choice {
        /// The chosen label (one of the question's criterion keys).
        label: String,
        /// Calibrated confidence of the pick (0.0–1.0).
        confidence: f64,
        /// Per-option probabilities, when reported.
        probabilities: BTreeMap<String, f64>,
    },
    /// A score answer: the expected level on the rubric scale + confidence.
    Score {
        /// The expected level (fractional, within the rubric's range).
        value: f64,
        /// Calibrated confidence of the answer (0.0–1.0).
        confidence: f64,
    },
    /// A yes/no answer: the calibrated probability of "true" (0.0–1.0).
    NoUl {
        /// P(true), 0.0–1.0.
        probability: f64,
    },
}

impl Answer {
    /// The calibrated confidence of the answer (0.0–1.0) — a choice/score
    /// carries an explicit confidence; a noul's `P(true)` is its confidence
    /// (Laya's own gating example gates on exactly this number). Callers
    /// apply their own threshold: below it, fall back to the pre-classifier
    /// behavior.
    pub fn confidence(&self) -> f64 {
        match self {
            Answer::Choice { confidence, .. } => *confidence,
            Answer::Score { confidence, .. } => *confidence,
            Answer::NoUl { probability } => *probability,
        }
    }
}

/// The live status of the classifier, surfaced to the UI. Off by default —
/// with Laya disabled (or unconfigured) the app behaves exactly as before.
///
/// Serialization is externally tagged (the default): unit variants serialize
/// as bare lowercase strings (`"disabled"`, …) matching the frontend string
/// comparisons (the [`EmbedderStatus`](super::EmbedderStatus) convention).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierStatus {
    /// Laya is off (the default) or enabled without an endpoint; no
    /// classifier call is ever attempted and no backend object exists.
    Disabled,
    /// Enabled with an endpoint; calls are attempted.
    Ready,
    /// The last call failed (unreachable endpoint, malformed response).
    /// Calls keep being attempted — a success flips back to `Ready` — while
    /// callers fall back on the `None` answer.
    Failed,
}

impl Default for ClassifierStatus {
    fn default() -> Self {
        Self::Disabled
    }
}

/// A classifier answers typed [`Question`]s about a state text with
/// calibrated probabilities.
///
/// Infallible by design, mirroring [`Embedder`](super::Embedder): `None`
/// means "no answer" — disabled, unreachable, or unparseable — and the
/// caller falls back to its pre-classifier behavior. A `Some` answer is only
/// as trustworthy as its [`Answer::confidence`]; gate on the caller's own
/// threshold, never on the mere presence of an answer.
#[async_trait]
pub trait Classifier: Send + Sync {
    /// Answer one [`Question`] about `state` (the text the question decides
    /// about — a prompt, a tool output, a memory's content). `None` = no
    /// answer. Never fails: a disabled or broken backend returns `None`.
    async fn classify(&self, state: &str, question: &Question) -> Option<Answer>;
}

/// The no-op classifier: never answers, never calls anything — an explicit
/// total [`Classifier`] for callers that want one (tests, or a default in
/// place of `Option`). This is NOT what the app builds while Laya is off:
/// disabled means no backend at all — `build_classifier` returns `None`,
/// which is the strictest form of "behaves exactly as before".
pub struct NoClassifier;

impl NoClassifier {
    /// Create the no-op classifier.
    pub fn new() -> Self {
        Self
    }
}

impl Default for NoClassifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Classifier for NoClassifier {
    async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
        None
    }
}

/// How long one classifier call may take before it is abandoned. Laya answers
/// a warm checkpoint in tens of milliseconds; the generous bound also covers a
/// cold CPU-side checkpoint load, while a wedged endpoint can never hang a
/// caller indefinitely. A timeout is a `None` answer + `Failed` status, like
/// every other failure.
pub const LAYA_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The Jev-compatible route `laya-serve` serves.
const LAYA_SYSTEMONE_PATH: &str = "/v1/systemone";

/// The single `questions` key every request uses: `laya-serve` keys its
/// `answers` by the question name the client sent, so one fixed name keeps the
/// request/response mapping inside this file.
const LAYA_QUESTION_KEY: &str = "question";

/// The Laya backend: `POST {endpoint}/v1/systemone`, Laya's Jev-compatible
/// wire protocol (see the module docs).
///
/// Failure-protected per the [`Classifier`] contract: a connection error, a
/// timeout, a non-success status, or an unparseable body all yield `None` and
/// flip the shared status to [`ClassifierStatus::Failed`] — no retries, no
/// panics, and no startup probe (constructing the client does no I/O). A
/// successful round-trip flips the status back to `Ready`.
pub struct LayaClassifier {
    /// One client per classifier, so connections are pooled across calls.
    client: reqwest::Client,
    /// Normalized `laya-serve` base URL (no trailing '/').
    endpoint: String,
    /// The shared status the IPC layer surfaces to Settings.
    status: Arc<RwLock<ClassifierStatus>>,
}

impl LayaClassifier {
    /// Build a Laya backend for `endpoint` — the `laya-serve` base URL, e.g.
    /// `http://127.0.0.1:8000` — with [`LAYA_REQUEST_TIMEOUT`] as the
    /// per-request bound.
    ///
    /// Fails only when the HTTP client itself cannot be built (for
    /// reqwest+rustls this essentially cannot happen); the caller
    /// (`build_classifier`) treats a failure as "no classifier", so the app
    /// keeps its pre-classifier behavior either way.
    pub fn new(
        endpoint: impl Into<String>,
        status: Arc<RwLock<ClassifierStatus>>,
    ) -> Result<Self, reqwest::Error> {
        Self::with_timeout(endpoint, status, LAYA_REQUEST_TIMEOUT)
    }

    /// Build a Laya backend with an explicit per-request timeout (the tests
    /// use a short one to exercise the timeout path).
    pub fn with_timeout(
        endpoint: impl Into<String>,
        status: Arc<RwLock<ClassifierStatus>>,
        timeout: Duration,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self {
            client,
            endpoint: endpoint.into().trim().trim_end_matches('/').to_string(),
            status,
        })
    }

    /// The `/v1/systemone` request body for one typed question about `state`:
    /// `{"state": <text>, "questions": {"question": <typed question>}}`.
    ///
    /// This and `parse_answer` are the complete Laya wire mapping, kept
    /// together so a protocol correction is a one-place change. `None` means
    /// the question could not be serialized — impossible for the current
    /// [`Question`] shape (strings, numbers and maps only) — handled rather
    /// than panicked on, per the failure-protected contract.
    fn systemone_request(state: &str, question: &Question) -> Option<serde_json::Value> {
        let question = serde_json::to_value(question).ok()?;
        let mut questions = serde_json::Map::new();
        questions.insert(LAYA_QUESTION_KEY.to_string(), question);
        Some(serde_json::json!({ "state": state, "questions": questions }))
    }

    /// Parse a `/v1/systemone` response body into the answer for `question`.
    ///
    /// The Jev-compatible answer shape is
    /// `{"answers": {"question": {"choice": "billing", "confidence": 0.94,
    /// "probabilities": {"billing": 0.94, "other": 0.06}}}}`; `score` and
    /// `noul` entries carry their primitive's value the same way, and the
    /// `{input_tokens, output_tokens}` usage block is ignored. A missing
    /// answer for our key, or a mismatched primitive, is `None` (the
    /// failure-protected path). A missing `confidence` reads as `0.0`:
    /// callers gate on it, so an unreported confidence is never trusted.
    fn parse_answer(question: &Question, body: &serde_json::Value) -> Option<Answer> {
        let entry = body.get("answers")?.get(LAYA_QUESTION_KEY)?;
        let confidence = entry
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        match question {
            Question::Choice { .. } => {
                let label = entry.get("choice")?.as_str()?.to_string();
                let probabilities = entry
                    .get("probabilities")
                    .and_then(|v| v.as_object())
                    .map(|map| {
                        map.iter()
                            .filter_map(|(k, v)| v.as_f64().map(|p| (k.clone(), p)))
                            .collect::<BTreeMap<String, f64>>()
                    })
                    .unwrap_or_default();
                Some(Answer::Choice {
                    label,
                    confidence,
                    probabilities,
                })
            }
            Question::Score { .. } => Some(Answer::Score {
                value: entry.get("score")?.as_f64()?,
                confidence,
            }),
            Question::NoUl { .. } => Some(Answer::NoUl {
                probability: entry.get("noul")?.as_f64()?,
            }),
        }
    }

    /// Flip the shared status. A poisoned lock means another thread panicked
    /// mid-write, so propagating the panic is the honest response.
    fn set_status(&self, status: ClassifierStatus) {
        *self.status.write().expect("classifier status lock poisoned") = status;
    }
}

#[async_trait]
impl Classifier for LayaClassifier {
    async fn classify(&self, state: &str, question: &Question) -> Option<Answer> {
        let body = match Self::systemone_request(state, question) {
            Some(body) => body,
            None => {
                self.set_status(ClassifierStatus::Failed);
                return None;
            }
        };
        let url = format!("{}{}", self.endpoint, LAYA_SYSTEMONE_PATH);
        // Every transport or protocol failure is the same outcome: `None`.
        let answer = async {
            let response = self.client.post(&url).json(&body).send().await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let payload: serde_json::Value = response.json().await.ok()?;
            Self::parse_answer(question, &payload)
        }
        .await;
        self.set_status(if answer.is_some() {
            ClassifierStatus::Ready
        } else {
            ClassifierStatus::Failed
        });
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn billing_choice() -> Question {
        Question::Choice {
            instructions: "Which department should handle this?".into(),
            criteria: BTreeMap::from([
                (
                    "billing".to_string(),
                    "invoices, payments, refunds".to_string(),
                ),
                ("other".to_string(), "everything else".to_string()),
            ]),
        }
    }

    #[tokio::test]
    async fn no_classifier_never_answers() {
        // The disabled default: no answer, so callers keep their
        // pre-classifier behavior.
        let classifier = NoClassifier::new();
        assert!(classifier
            .classify("billed twice, refund please", &billing_choice())
            .await
            .is_none());
    }

    #[test]
    fn classifier_status_defaults_to_disabled_and_serializes_lowercase() {
        assert_eq!(ClassifierStatus::default(), ClassifierStatus::Disabled);
        // The frontend compares plain lowercase strings.
        assert_eq!(
            serde_json::to_value(ClassifierStatus::Disabled).unwrap(),
            serde_json::json!("disabled")
        );
        assert_eq!(
            serde_json::to_value(ClassifierStatus::Ready).unwrap(),
            serde_json::json!("ready")
        );
        assert_eq!(
            serde_json::to_value(ClassifierStatus::Failed).unwrap(),
            serde_json::json!("failed")
        );
    }

    #[test]
    fn questions_serialize_to_the_laya_wire_shape() {
        assert_eq!(
            serde_json::to_value(billing_choice()).unwrap(),
            serde_json::json!({
                "type": "choice",
                "instructions": "Which department should handle this?",
                "criteria": {
                    "billing": "invoices, payments, refunds",
                    "other": "everything else"
                }
            })
        );
        let score = Question::Score {
            instructions: "How urgent is this?".into(),
            criteria: vec!["not urgent".into(), "soon".into(), "critical".into()],
        };
        assert_eq!(
            serde_json::to_value(&score).unwrap(),
            serde_json::json!({
                "type": "score",
                "instructions": "How urgent is this?",
                "criteria": ["not urgent", "soon", "critical"]
            })
        );
        let noul = Question::NoUl {
            instructions: "Does the user threaten to cancel?".into(),
            criteria: None,
        };
        assert_eq!(
            serde_json::to_value(&noul).unwrap(),
            serde_json::json!({
                "type": "noul",
                "instructions": "Does the user threaten to cancel?"
            })
        );
        // A noul with explicit option-text overrides carries them.
        let labelled = Question::NoUl {
            instructions: "Is this review positive?".into(),
            criteria: Some(BTreeMap::from([
                (
                    "true".to_string(),
                    "yes, the review is positive".to_string(),
                ),
                (
                    "false".to_string(),
                    "no, the review is negative".to_string(),
                ),
            ])),
        };
        assert_eq!(
            serde_json::to_value(&labelled).unwrap(),
            serde_json::json!({
                "type": "noul",
                "instructions": "Is this review positive?",
                "criteria": {
                    "true": "yes, the review is positive",
                    "false": "no, the review is negative"
                }
            })
        );
    }

    #[test]
    fn answer_confidence_reads_the_primitive() {
        let choice = Answer::Choice {
            label: "billing".into(),
            confidence: 0.94,
            probabilities: BTreeMap::from([("billing".to_string(), 0.94)]),
        };
        assert_eq!(choice.confidence(), 0.94);
        let score = Answer::Score {
            value: 1.84,
            confidence: 0.7,
        };
        assert_eq!(score.confidence(), 0.7);
        let noul = Answer::NoUl { probability: 0.892 };
        assert_eq!(noul.confidence(), 0.892);
    }

    #[test]
    fn answers_round_trip_through_serde() {
        let answer = Answer::Choice {
            label: "billing".into(),
            confidence: 0.94,
            probabilities: BTreeMap::from([("billing".to_string(), 0.94)]),
        };
        let json = serde_json::to_string(&answer).unwrap();
        assert_eq!(serde_json::from_str::<Answer>(&json).unwrap(), answer);
    }

    // ---- Laya HTTP backend: one-shot stub-server round trips --------------
    //
    // The stub mirrors `src/provider/openai/tests.rs` — a raw
    // `tokio::net::TcpListener` speaking just enough HTTP/1.1. The repo has no
    // mock-HTTP crates, and these tests need none.

    /// One request the stub server received.
    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        body: serde_json::Value,
    }

    /// A one-shot HTTP stub: serves one canned response (or never answers, for
    /// the timeout test) and records the request it saw.
    struct StubServer {
        addr: std::net::SocketAddr,
        request: Arc<std::sync::Mutex<Option<RecordedRequest>>>,
    }

    impl StubServer {
        /// Serve `status` + `body` for the first request, then stop.
        async fn start(status: u16, body: &'static str) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind stub server");
            let addr = listener.local_addr().expect("stub server addr");
            let request = Arc::new(std::sync::Mutex::new(None));
            let recorded = Arc::clone(&request);
            tokio::spawn(async move {
                let (mut socket, _peer) = listener.accept().await.expect("accept");
                // Read the request head, then the body Content-Length promises.
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.expect("read head");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head_end = buf
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4)
                    .expect("request head terminator");
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let content_length: usize = head
                    .lines()
                    .find_map(|l| {
                        let lower = l.to_ascii_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + content_length {
                    let n = socket.read(&mut chunk).await.expect("read body");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let mut request_line = head.lines().next().unwrap_or_default().split_whitespace();
                let method = request_line.next().unwrap_or_default().to_string();
                let path = request_line.next().unwrap_or_default().to_string();
                let parsed = serde_json::from_slice(&buf[head_end..head_end + content_length])
                    .unwrap_or(serde_json::Value::Null);
                *recorded.lock().expect("request lock") =
                    Some(RecordedRequest { method, path, body: parsed });

                let reason = if status == 200 { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
                socket.shutdown().await.expect("shutdown");
            });
            Self { addr, request }
        }

        /// Accept one connection and never answer it — the client must hit its
        /// own timeout. Held far longer than any test's timeout.
        async fn start_hanging() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind stub server");
            let addr = listener.local_addr().expect("stub server addr");
            tokio::spawn(async move {
                let (_socket, _peer) = listener.accept().await.expect("accept");
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
            Self {
                addr,
                request: Arc::new(std::sync::Mutex::new(None)),
            }
        }

        /// The `laya-serve`-style base URL for this stub.
        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        /// The request received so far, if any.
        fn recorded(&self) -> Option<RecordedRequest> {
            self.request.lock().expect("request lock").clone()
        }
    }

    /// A Laya backend pointed at `endpoint` with `timeout`, on a fresh status.
    fn laya_classifier(
        endpoint: &str,
        timeout: Duration,
    ) -> (LayaClassifier, Arc<RwLock<ClassifierStatus>>) {
        let status = Arc::new(RwLock::new(ClassifierStatus::Disabled));
        let classifier = LayaClassifier::with_timeout(endpoint, Arc::clone(&status), timeout)
            .expect("build Laya classifier");
        (classifier, status)
    }

    #[tokio::test]
    async fn laya_classifier_round_trips_a_choice_question() {
        // The acceptance test from backlog bb54bdcc: with Laya enabled, a
        // choice question round-trips end to end against a stub endpoint.
        let server = StubServer::start(
            200,
            r#"{"model":"english","answers":{"question":{"choice":"billing","confidence":0.94,"probabilities":{"billing":0.94,"other":0.06}}},"usage":{"input_tokens":9,"output_tokens":2}}"#,
        )
        .await;
        // A trailing slash on the configured endpoint must not double up.
        let (classifier, status) =
            laya_classifier(&format!("{}/", server.base_url()), Duration::from_secs(5));

        let answer = classifier
            .classify("billed twice, refund please", &billing_choice())
            .await
            .expect("a choice answer");
        assert_eq!(
            answer,
            Answer::Choice {
                label: "billing".into(),
                confidence: 0.94,
                probabilities: BTreeMap::from([
                    ("billing".to_string(), 0.94),
                    ("other".to_string(), 0.06),
                ]),
            }
        );
        // A successful round-trip flips the shared status to Ready.
        assert_eq!(*status.read().expect("status lock"), ClassifierStatus::Ready);

        // The stub saw the pinned Jev-compatible wire shape.
        let request = server.recorded().expect("recorded request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/systemone");
        assert_eq!(
            request.body,
            serde_json::json!({
                "state": "billed twice, refund please",
                "questions": {
                    "question": {
                        "type": "choice",
                        "instructions": "Which department should handle this?",
                        "criteria": {
                            "billing": "invoices, payments, refunds",
                            "other": "everything else"
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn laya_parse_maps_each_primitive() {
        // The response mapping, pinned per primitive (the Jev-compatible
        // `answers` shape `laya-serve` returns).
        let choice = billing_choice();
        let body = serde_json::json!({
            "answers": {"question": {"choice": "billing", "confidence": 0.94,
                "probabilities": {"billing": 0.94, "other": 0.06}}}
        });
        assert_eq!(
            LayaClassifier::parse_answer(&choice, &body),
            Some(Answer::Choice {
                label: "billing".into(),
                confidence: 0.94,
                probabilities: BTreeMap::from([
                    ("billing".to_string(), 0.94),
                    ("other".to_string(), 0.06),
                ]),
            })
        );

        let score = Question::Score {
            instructions: "How urgent is this?".into(),
            criteria: vec!["not urgent".into(), "soon".into(), "critical".into()],
        };
        // A missing confidence reads as 0.0 — callers gate on it, so an
        // unreported confidence is never trusted.
        let body = serde_json::json!({"answers": {"question": {"score": 1.5}}});
        assert_eq!(
            LayaClassifier::parse_answer(&score, &body),
            Some(Answer::Score {
                value: 1.5,
                confidence: 0.0,
            })
        );

        let noul = Question::NoUl {
            instructions: "Does the user threaten to cancel?".into(),
            criteria: None,
        };
        let body = serde_json::json!({"answers": {"question": {"noul": 0.892}}});
        assert_eq!(
            LayaClassifier::parse_answer(&noul, &body),
            Some(Answer::NoUl { probability: 0.892 })
        );

        // Failure-protected: no answer for our key, a mismatched primitive, or
        // no `answers` block at all ⇒ None.
        assert_eq!(
            LayaClassifier::parse_answer(&choice, &serde_json::json!({"answers": {}})),
            None
        );
        assert_eq!(
            LayaClassifier::parse_answer(
                &choice,
                &serde_json::json!({"answers": {"question": {"noul": 0.5}}})
            ),
            None
        );
        assert_eq!(
            LayaClassifier::parse_answer(&choice, &serde_json::json!({})),
            None
        );
    }

    #[tokio::test]
    async fn laya_malformed_response_yields_none_and_failed_status() {
        let server = StubServer::start(200, "not json at all").await;
        let (classifier, status) = laya_classifier(&server.base_url(), Duration::from_secs(5));
        assert!(classifier
            .classify("billed twice", &billing_choice())
            .await
            .is_none());
        assert_eq!(
            *status.read().expect("status lock"),
            ClassifierStatus::Failed
        );
    }

    #[tokio::test]
    async fn laya_http_error_yields_none_and_failed_status() {
        let server = StubServer::start(500, r#"{"detail":"inference failed"}"#).await;
        let (classifier, status) = laya_classifier(&server.base_url(), Duration::from_secs(5));
        assert!(classifier
            .classify("billed twice", &billing_choice())
            .await
            .is_none());
        assert_eq!(
            *status.read().expect("status lock"),
            ClassifierStatus::Failed
        );
    }

    #[tokio::test]
    async fn laya_timeout_yields_none_and_failed_status() {
        let server = StubServer::start_hanging().await;
        let (classifier, status) = laya_classifier(&server.base_url(), Duration::from_millis(200));
        assert!(classifier
            .classify("billed twice", &billing_choice())
            .await
            .is_none());
        assert_eq!(
            *status.read().expect("status lock"),
            ClassifierStatus::Failed
        );
    }

    #[tokio::test]
    async fn disabled_path_never_calls_http() {
        // The hard requirement's shape: with no Laya backend to route through
        // (disabled or unconfigured — `build_classifier` returns `None`;
        // `NoClassifier` stands in here as the explicit no-op), answering a
        // question opens no connection at all.
        let server = StubServer::start(200, r#"{"answers":{}}"#).await;
        let classifier = NoClassifier::new();
        assert!(classifier
            .classify("billed twice", &billing_choice())
            .await
            .is_none());
        // Give a would-be request time to arrive before declaring silence.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(server.recorded().is_none());
    }
}
