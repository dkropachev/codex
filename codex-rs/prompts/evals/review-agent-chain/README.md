# Review agent-chain prompt evaluation

## Outcome

Across two seeded runs, all three arms found all eight seeded bugs and left all
four controls clean on human review. The automated lexical scorer reports a
lower result for the staged arms because one correct SinglePass report misses a
keyword threshold and one DoubleCheck report adds a plausible issue outside the
single-finding oracle.

SinglePass was 24.7% faster than `main`. DoubleCheck matched `main` latency,
verified every seeded finding as introduced by the change, and produced one
additional P2 finding.

## Method

- Model: `gpt-5.6-sol` with medium reasoning for every arm.
- Target: uncommitted changes in six independent repositories under 30 source
  lines each.
- Arms: `origin/main`, optimized SinglePass, and optimized DoubleCheck.
- Cases: pagination data loss, tenant-cache isolation, pre-existing control,
  intentional documented behavior, rollout compatibility, and duplicate-root
  collision.
- Two randomized sequential runs per case and arm, 36 runs total, with no
  retries.
- Oracles were stored outside each checkout. Four cases contain one seeded P1
  bug; two are controls.
- The benchmark covers Report only. Integration tests cover Fix and Fix+commit.

Run the matrix with `run_matrix.py`, then score its artifact with `score.py`.
The checked-in `results.json` is the raw 36-run artifact used below.

## Automated score

| Metric | `main` | SinglePass | DoubleCheck |
|---|---:|---:|---:|
| True positives | 8 | 7 | 8 |
| False positives | 0 | 1 | 1 |
| False negatives | 0 | 1 | 0 |
| Precision / recall / F1 | 1.000 / 1.000 / 1.000 | 0.875 / 0.875 / 0.875 | 0.889 / 1.000 / 0.941 |
| Correct finding decisions | 12/12 | 12/12 | 12/12 |
| Correct explicit verdicts | n/a | 12/12 | 12/12 |
| Exact P1 | 8 | 5 | 5 |
| Priority within one level | 8 | 7 | 8 |
| Exact introduced classification | n/a | n/a | 8/8 |
| Duplicate findings | 0 | 0 | 0 |
| Mean wall time | 23.728 s | 17.863 s | 23.505 s |
| Total report words | 677 | 561 | 636 |

The SinglePass pagination report says `items[:-1]` “discards the final
collected item.” The scorer requires at least two terms from
`last | final | drop`, so it counts that correct finding once as a false
negative and again as a false positive. Human review gives SinglePass 8/8
seeded findings with no extra findings.

DoubleCheck's second tenant-cache run reports the seeded cross-tenant key bug
and a separate P2 for unbounded growth in the new process-wide cache. The
second issue is concrete but is outside the single-finding oracle, so the
scorer counts it as a false positive. All four control runs remain clean.

The eight seeded SinglePass findings contain six P1 and two P2 priorities. The
eight seeded DoubleCheck findings contain five P1 and three P2 priorities. All
are within one level of the P1 oracle.

## Fixed-prompt size

Token counts use the repository's conservative `ceil(bytes / 4)` estimate.
Stage rows include the shared rules.

| Prompt | Initial chain | Optimized | Change |
|---|---:|---:|---:|
| Shared rules | 730 B / 183 tok | 494 B / 124 tok | -32% |
| Review | 1,689 B / 423 tok | 1,591 B / 398 tok | -6% |
| Double-check | 2,236 B / 559 tok | 1,796 B / 449 tok | -20% |
| Fix scope | n/a | 1,309 B / 328 tok | n/a |
| Fix | 1,819 B / 455 tok | 1,380 B / 345 tok | -24% |
| Fix+commit | 1,938 B / 485 tok | 1,490 B / 373 tok | -23% |
| Repair fixed text | 1,154 B / 289 tok | 926 B / 232 tok | -20% |

The optimized Review instruction is 78.2% smaller than `main`'s 7,307-byte
rubric. Including the 1,850-byte discovery schema, SinglePass uses about 861
fixed tokens, 52.9% below `main`'s estimated 1,827 tokens. DoubleCheck adds its
1,796-byte instruction and 1,725-byte schema for about 1,742 fixed tokens
across two calls, 4.7% below `main` before dynamic candidate and source
context.

Repair does not repeat the response schema in prompt text because the Responses
API already enforces that schema.

## Limits

- Six synthetic cases with two stochastic runs are a smoke evaluation, not a
  statistical model study.
- The lexical scorer uses keyword presence, treats every extra positive-case
  finding as false, and can count one mismatch as both a false negative and a
  false positive.
- `main` uses its legacy report format, so the scorer cannot grade an explicit
  assessment verdict for that arm.
- Live usage counters were null for all 36 runs. Prompt-size comparisons cover
  fixed instruction and schema text, not repository context, tool output,
  caching, output tokens, or cost.
- Wall time includes process startup and network variance.
- The corpus does not exercise Fix mutations.
