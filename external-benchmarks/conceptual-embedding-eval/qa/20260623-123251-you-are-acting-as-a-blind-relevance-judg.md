# Q&A — 20260623-123251

## Question

You are acting as a BLIND relevance judge for a French legal search-quality evaluation.

Working folder: /home/pierre/Work/jurisearch/external-benchmarks/conceptual-embedding-eval/

INPUT — read this file:
  /home/pierre/Work/jurisearch/external-benchmarks/conceptual-embedding-eval/judge_input.json

It is a JSON array. Each element is:
  { "question_id": "<id>", "question": "<a lay French legal question>",
    "candidates": [ { "key": "c01", "title": "...", "snippet": "..." }, ... ] }

The candidates are pooled retrieval results. You are NOT told which search engine returned which
candidate, and there is no "correct" answer key — judge purely on substance.

TASK — for EVERY question, judge EVERY candidate's relevance to that question, using ONLY its
title + snippet, on this graded scale:
  2 = directly relevant — this article squarely answers the question, or is exactly the provision
      a person asking this question is looking for.
  1 = related / partial — same legal topic or adjacent; touches the question but does not directly
      answer it (e.g., a neighbouring article, a general principle, the right area but wrong point).
  0 = unrelated — different topic; would not help answer the question.

Judge on MEANING, not keyword overlap: a snippet can repeat the question's words yet be off-topic
(score 0), and a snippet can answer it with different words (score 2). These are French statutory
articles; reason about what the article actually governs.

OUTPUT — write a single JSON object to this exact path:
  /home/pierre/Work/jurisearch/external-benchmarks/conceptual-embedding-eval/judge_output.json
Shape:
  { "<question_id>": { "c01": 0|1|2, "c02": 0|1|2, ... }, ... }
Requirements:
  - Include EVERY question_id from the input, and EVERY candidate key under each question.
  - Values are integers 0, 1, or 2 only.
  - Valid JSON, no comments, no prose, no trailing commas.
Do not skip any question or any candidate. If unsure between two scores, pick the lower one.

## Answer

DONE
