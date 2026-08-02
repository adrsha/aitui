# Phase 2–3 plan: faster, verified multi-agent orchestration

## Design target

Preserve the reducer/effects architecture and current permission semantics while changing the critical path from mostly serial execution into bounded parallel waves with deterministic ordering and independent verification.

```text
Parent decomposition
  ├─ read-only tool wave ───────────────┐
  ├─ child branch / replicas ───────────┼─ barrier / evidence validation
  └─ other independent child branches ─┘              │
                                                       ▼
                                  compact consensus + disagreement digest
                                                       │
                                   independent verifier where required
                                                       │
                                                       ▼
                                             parent integration
                                                       │
                                             independent final checks
```

## 1. Add a deterministic baseline harness

**Maps to H and the Phase 3 measurement requirement.**

Add an orchestration evaluation module with a controllable fake executor and clock-independent event traces. It will capture the current baseline before scheduler changes and remain as regression coverage afterward.

Representative latency cases:

1. Three independent read-only calls with known delays. Current expected critical path: approximately the sum; optimized target: approximately the maximum.
2. Read → mutation → read. Both reads must remain on opposite sides of the mutation barrier.
3. Three child completions arriving out of order. Parent must resume once, only after all required children finish.
4. Cached speculative result plus two asynchronous reads. Commit order must still match call order.
5. Cancellation and stale late completion.

Representative correctness/eval cases:

- Agreement with valid evidence.
- Same answer with stale or invalid evidence.
- Two conflicting answers.
- One failure plus one supported answer.
- Two-way disagreement resolved by a third replica.
- Unresolved disagreement sent to an independent verifier.
- Malformed verifier output fails closed to `unknown`.
- Prompt-injection text inside evidence remains untrusted data.

Baseline metrics will be checked into a JSON report generated from the pre-change scheduler and compared with the same fixtures after each stage. Live-model timing may be reported separately if a configured endpoint is available, but deterministic CI will not depend on network inference variance.

## 2. Parallelize approved read-only ordinary tools

**Maps to A, E, G, and H.**

Introduce an ordered `ToolBatchBarrier` in app state and a result channel carrying batch ID, source index, owning session, and `ToolResult`.

- Batch only already-authorized `read`, `list`, `search`, `web search/images/reverse-image/fetch` calls.
- Never move calls across todo, interaction, child-agent, shell, download, mutation, unknown-tool, or permission barriers.
- Consume permission rules once per call before launch.
- Reuse speculative results immediately and spawn only cache misses.
- Buffer completion results and commit transcript/tool messages in source order.
- Continue the parent exactly once after all slots are terminal.
- Abort workers and reject stale batch IDs on cancellation.
- Start with a bounded concurrency limit to avoid endpoint and blocking-pool contention.

This is deliberately conservative: independent writes will not be parallelized until there is an explicit read/write conflict model.

## 3. Make child outputs structured and lean

**Maps to B, E, and F.**

Add `agent/report.rs` with bounded, serde-validated report types:

- Explicit check IDs supplied by the parent.
- Typed answers: `yes`, `no`, `mixed`, `unknown`.
- Concise statements.
- Evidence references for file lines, command outputs, and web URLs.
- Explicit partial/blocked status and uncertainties.

Child contexts remain isolated: system prompt + delegated task/checks + cwd only. They will not inherit parent history or sibling reports.

Validate local evidence deterministically where possible:

- File exists and remains inside cwd.
- Cited line range exists.
- Quote occurs in that range.
- Duplicate or unknown check IDs are rejected.
- Report size, finding count, and evidence excerpts are bounded.

The parent transcript/API context receives a compact deterministic digest. Raw reports and progress logs remain available in task state/UI but do not enter normal synthesis context.

Backward compatibility: legacy free-form child calls continue to work. Structured checks are enabled when the workflow call supplies `checks` or a verification policy.

## 4. Add automatic replicas, voting, and disagreement escalation

**Maps to C and F.**

Extend `workflow(action: "agent")` with optional:

```json
{
  "checks": [{"id": "...", "question": "..."}],
  "verification": "none|replicate|strict"
}
```

Policies:

- `none`: one child, preserving current behavior.
- `replicate`: two isolated replicas run concurrently.
- `strict`: two replicas plus independent evidence verification for accepted load-bearing findings.

Voting is deterministic and evidence-gated:

- Matching supported yes/no answers produce provisional consensus.
- `mixed`, `unknown`, missing findings, invalid evidence, failures, or differing yes/no answers create a disagreement.
- Free-text similarity and self-reported confidence never decide a vote.
- A disagreement launches a third isolated replica with the original task only.
- Remaining disagreement launches an independent verifier.
- Unresolved or malformed results remain explicitly `unknown`; they are never silently averaged.

## 5. Add an independent verifier

**Maps to D, E, F, and G.**

The verifier is a separate model invocation with a different prompt and no access to the producing agents’ reasoning or parent transcript. It receives only:

- Original task and disputed success criterion.
- Compact typed claims.
- Evidence locators/excerpts treated as untrusted.
- Read-only tools needed to re-read or re-run checks.

It returns a bounded typed adjudication. File/command evidence is revalidated mechanically before acceptance. Verifier failure resolves to `unknown` and is surfaced to the parent.

For code-changing tasks, the parent remains responsible for mutation, followed by separate final verification calls that inspect the resulting artifact and execute tests rather than asking the implementing model to grade itself.

## 6. Shorten child scheduling depth without guessing dependencies

**Maps to A, G, and H.**

First, retain the safe rule that consecutive child calls form one wave, but add explicit caps and structured worker state. Then add optional `depends_on` metadata for dependency-aware child directed acyclic graphs within one parent tool round.

- Nodes with no unmet dependencies launch immediately, bounded by concurrency.
- Nodes launch in waves as prerequisites finish.
- Cycles, self-dependencies, and invalid indexes fail deterministically.
- Reports remain in declaration order.
- A failed prerequisite causes dependents to be skipped with an explicit dependency failure rather than treating bad output as ground truth.

The scheduler will not infer dependencies from natural-language prompts or `task_index`; that would create silent correctness risk.

## 7. Address cancellation and race safety

**Maps to A, G, and H.**

- Store abort handles for ordinary tool batches.
- Ignore late results by batch/generation ID.
- Ensure cancellation cannot restart the model after a late `spawn_blocking` result.
- Route all results to their owning session rather than the currently active session.
- Add stress tests with randomized completion order, duplicate events, session switches, and cancellation.

## 8. Documentation and final validation

Update `ARCHITECTURE.md`, `DECISIONS.md`, `FEATURES.md`, and `ROADMAP.md` as each stage lands.

Before completion run:

- Baseline/after orchestration evaluation suite.
- `cargo fmt -- --check`.
- `cargo clippy --all-targets -- -D warnings`.
- `cargo test`.
- Repeated race/order tests.

The final report will include measured critical-path changes, correctness score changes on the fixture eval set, unresolved tradeoffs, and any latency floor that remains.

## Genuine tradeoffs

1. **Concurrency cap:** unlimited fan-out can worsen wall-clock time under API throttling or Tokio blocking-pool saturation. A bounded default is faster in realistic conditions, but the best value is endpoint-dependent. The implementation will make the cap explicit and benchmark representative values.
2. **Buffered transcript ordering:** retaining source-order tool messages means a fast result may not become API-visible until the slowest sibling completes. UI progress can still update immediately; parent synthesis correctly waits for the full wave.
3. **Independent writes:** parallelizing them requires a real path conflict model and filesystem snapshot semantics. They remain sequential initially.
4. **Verification adds a true inference hop:** strict verification intentionally increases the critical path. Replicas run in parallel; the verifier is only sequential when it genuinely depends on candidate reports.
5. **Cross-session parent rounds:** fully concurrent per-session orchestration requires moving global pending/permission/barrier state into per-session round objects and coordinating one shared human permission overlay. This is valuable but higher-blast-radius, so it follows the within-round scheduler and verification work rather than landing first.