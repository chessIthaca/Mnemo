// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useState, useEffect } from "react";
import { MessageCircleQuestion } from "lucide-react";
import type { PendingQuestion } from "../../hooks/useAgentStore";
import { useAgentStore } from "../../hooks/useAgentStore";
import { answerQuestion } from "../../lib/tauri";

interface QuestionPromptProps {
  question: PendingQuestion;
}

/**
 * Renders an `ask_user` question as a vertical list of **numbered** choices:
 * each real option as `N) <label>` (with an optional description below), then a
 * final `M) 💬 Let's talk about it` choice (M = options.length + 1). There is no
 * inline freeform textarea in the prompt — picking the "Let's talk about it"
 * number switches the standard InputBar into freeform-answer mode (the user
 * types the answer there).
 *
 * Numbering is `1)`, `2)`, … (digit(s) + `)`), matching the syntax the user can
 * type in the InputBar to resolve the question by choice index.
 *
 * Once answered (confirmed by the backend), the live dialog **disappears** —
 * the full Q→A persists as a `qa` transcript entry (appended by the store's
 * `recordQuestionAnswer`), shown as static text right where the prompt was.
 * `recordQuestionAnswer` clears `pendingQuestion` itself on answer (the
 * backend never emits a fresh `started` for a mid-turn `ask_user`), so the
 * prompt unmounts immediately via the parent's `pendingQuestion` gate; the
 * `pendingQuestionAnswered` marker below is a belt-and-suspenders guard for
 * the same id.
 *
 * Mirrors `ApprovalPrompt` in shape: a `resolveError` for a failed/expired
 * answer. The `key={questionId}` reset behavior is preserved by the parent.
 */
export function QuestionPrompt({ question }: QuestionPromptProps) {
  const recordQuestionAnswer = useAgentStore((s) => s.recordQuestionAnswer);
  const setFreeformQuestion = useAgentStore((s) => s.setFreeformQuestion);
  const activeAgent = useAgentStore((s) => s.activeAgent);
  // The answered marker for THIS agent — hides the prompt the instant the
  // answer is confirmed, so the `qa` transcript entry (appended on answer)
  // takes over without a duplicated live prompt.
  const answeredId = useAgentStore((s) =>
    activeAgent !== null ? s.agents[activeAgent]?.pendingQuestionAnswered ?? null : null,
  );
  const [resolveError, setResolveError] = useState<string | null>(null);

  // Reset local error state when the question changes (a new ask_user call),
  // and clear any stale freeform-answer mode so the InputBar starts fresh.
  useEffect(() => {
    setResolveError(null);
    if (activeAgent !== null) {
      setFreeformQuestion(activeAgent, null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [question.questionId]);

  // Answered → render nothing. The `qa` transcript entry (appended by the
  // store on confirmed answer) shows the Q→A as static text in the
  // conversation, right where the prompt was. This avoids duplicating the
  // Q→A between a collapsed prompt and the transcript entry.
  if (answeredId === question.questionId) {
    return null;
  }

  /** Resolve the question by a 0-indexed choice (confirmed by the backend). */
  async function answerChoice(index: number, label: string) {
    setResolveError(null);
    const ok = await answerQuestion(question.questionId, {
      kind: "choice",
      index,
    });
    if (ok) {
      // Confirmed — record the Q→A (appends the `qa` transcript entry + sets
      // the answered marker, which hides this prompt on the next render).
      if (activeAgent !== null) {
        recordQuestionAnswer(activeAgent, {
          questionId: question.questionId,
          question: question.question,
          answer: label,
        });
      }
    } else {
      setResolveError("Question expired or already answered.");
    }
  }

  /** Switch the InputBar into freeform-answer mode for this question. */
  function requestFreeform() {
    if (activeAgent !== null) {
      setFreeformQuestion(activeAgent, question.questionId);
    }
  }

  // Live (unanswered) view — numbered choices.
  const freeformNumber = question.options.length + 1;

  return (
    <div className="my-2 rounded-lg border border-cyan-600/40 bg-cyan-950/20 px-4 py-3">
      <div className="mb-2 flex items-center gap-2 text-xs font-medium text-cyan-300">
        <MessageCircleQuestion className="h-4 w-4 shrink-0" />
        <span>The agent has a question</span>
      </div>
      <p className="mb-3 text-sm text-slate-100">{question.question}</p>

      {/* Numbered choices: real options, then "Let's talk about it". */}
      <div className="flex flex-col gap-1.5">
        {question.options.map((opt, i) => (
          <button
            key={i}
            type="button"
            onClick={() => void answerChoice(i, opt.label)}
            className="group flex items-start gap-2 rounded-lg border border-border bg-bg-primary px-3 py-2 text-left text-sm text-slate-200 transition-colors hover:border-cyan-500/60 hover:bg-cyan-950/30"
          >
            <span className="shrink-0 font-medium text-cyan-400">{i + 1})</span>
            <span className="flex flex-col gap-0.5">
              <span className="font-medium">{opt.label}</span>
              {opt.description && (
                <span className="text-xs text-slate-400">{opt.description}</span>
              )}
            </span>
          </button>
        ))}
        {/* The freeform "Let's talk about it" choice — its own number. Picking
            it switches the InputBar into freeform-answer mode (no inline box). */}
        <button
          type="button"
          onClick={requestFreeform}
          className="group flex items-start gap-2 rounded-lg border border-border bg-bg-primary px-3 py-2 text-left text-sm text-slate-200 transition-colors hover:border-cyan-500/60 hover:bg-cyan-950/30"
        >
          <span className="shrink-0 font-medium text-cyan-400">{freeformNumber})</span>
          <span className="flex items-center gap-1.5">
            <span>💬 Let&apos;s talk about it</span>
          </span>
        </button>
      </div>

      {resolveError && (
        <div className="mt-2 text-xs text-red-300">{resolveError}</div>
      )}
    </div>
  );
}
