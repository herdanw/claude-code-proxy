# Sessions, Requests, Correlation Links, Top Explanations, and Session Timeline — Architecture Analysis Design

**Date:** 2026-04-11  
**Status:** Approved (design phase)

## 1) Objective and Scope

Produce a spec-grade architecture analysis of how the system currently uses sessions and requests, and how the derived intelligence surfaces (Correlation Links, Top Explanations, Session Timeline) are produced, persisted, queried, and rendered.

Target outcome (user-selected):
- Full architecture handoff quality
- Full operational/debug mental model
- Full feature-planning leverage
- Plus prioritized, actionable recommendations

In scope:
- Runtime flow from request ingest through enrichment and UI exposure
- Data contracts and endpoint boundaries
- Ordering/truncation/consistency behavior
- Failure modes and test coverage anchors
- Improvement sequencing

Out of scope:
- Implementing code changes in this phase
- Refactoring unrelated modules
- Altering API behavior without a separate implementation plan

## 2) System Crux

These domains are one connected pipeline, not separate subsystems:
- **Requests** are the atomic source of truth.
- **Sessions** are the aggregation lens over proxy + local evidence.
- **Correlation Links** are evidence edges from request to local events.
- **Top Explanations** are ranked hypotheses about likely causes.
- **Session Timeline** is the fused chronological investigation surface.

## 3) Architecture Frame (End-to-End)

1. **Ingest + persist request**
   - Request telemetry is captured and written through `StatsStore::add_entry` (`src/stats.rs:400`).
   - Feed/stats APIs expose request history and aggregates (`/api/entries`, `/api/stats` in `src/dashboard.rs:41,40`).

2. **Derive intelligence per request**
   - Correlation engine computes request↔local-event links (`correlate_request`, `src/correlation_engine.rs:22`) and persists replacement sets via `replace_correlations_for_request` (`src/stats.rs:2100`).
   - Explainer computes ranked hypotheses (`explain_request`, `src/explainer.rs:11`) and persists via `replace_explanations_for_request` (`src/stats.rs:2324`).

3. **Aggregate by session identity**
   - Session detail/graph views synthesize proxy requests and local artifacts (`get_session_details`, `src/stats.rs:959`; `get_session_graph`, `src/stats.rs:1176`).

4. **Render investigation surfaces**
   - Correlations: `/api/correlations` (`src/dashboard.rs:43,306`)
   - Explanations: `/api/explanations` (`src/dashboard.rs:44,314`)
   - Timeline: `/api/timeline` (`src/dashboard.rs:45,322`)
   - Session graph: `/api/session-graph` (`src/dashboard.rs:46,378`)
   - Session details: `/api/session-details` (`src/dashboard.rs:47,477`)

## 4) Domain Analysis

### 4.1 Requests (Atomic Substrate)

**Purpose**
- Canonical event stream backing all higher-level analysis and UI.

**Canonical data behavior**
- Entries include timing/status/tokens/stalls/session_id and anomaly metadata.
- Request ingest persists before enrichment, preserving core observability even if enrichment fails.

**Write path**
- `add_entry` persists and broadcasts (`src/stats.rs:400`).
- Correlation and explanation replacement writes happen afterward (`src/main.rs:239-252`, `src/main.rs:274-280`).

**Read path**
- Request feed + filtering/pagination: `get_entries`, `get_entries_with_anomaly_focus` (`src/stats.rs:618,653`).
- Stats snapshots: `get_live_stats_snapshot`, `get_historical_stats_snapshot` (`src/stats.rs:456,468`).

**Invariants / failure behavior**
- Enrichment failures must not block request persistence.
- Anomaly-focus ordering and pagination must remain deterministic.
- Null/unknown session request rows remain first-class query citizens.

**Test anchors**
- `stats` tests for full-history reads, anomaly focus ordering/pagination/preselection stability, and reset behavior.

---

### 4.2 Sessions (Aggregation + Identity Layer)

**Purpose**
- Convert request stream into session-level investigative units with proxy/local presence semantics.

**Canonical data behavior**
- Session presence derives from union of proxy activity and local evidence.
- Session details include requests, timeline, conversation summaries, and truncation metadata.

**Write path**
- Indirect: request writes and local context ingesters populate source data.
- No separate mutable “session state table” as primary truth.

**Read path**
- `/api/sessions` and `/api/claude-sessions` feed merged UI list (`src/dashboard.rs:42,54`).
- `/api/session-details` exposes deep payload (`src/dashboard.rs:47,477`; `src/stats.rs:959`).

**Invariants / failure behavior**
- Unknown sessions must return explicit none/not-found semantics.
- Deletion safety uses liveness guard checks.
- Proxy-only/local-only/both presence states must be stable and testable.

**Test anchors**
- Session presence union logic, ordering/limits, unknown-session behavior, delete live-guard behavior, large-session delete scale.

---

### 4.3 Correlation Links (Causal Evidence Layer)

**Purpose**
- Attach local evidence likely related to a specific request.

**Canonical model**
- `RequestCorrelation` fields: request id, local event id, link type, confidence, reason, timestamp (`src/stats.rs:278`).

**Write path**
- `correlate_request` computes links (`src/correlation_engine.rs:22`).
- `replace_correlations_for_request` atomically replaces request-scoped set (`src/stats.rs:2100`).

**Read path**
- `get_correlations_for_request` and `/api/correlations` (`src/stats.rs:2159`, `src/dashboard.rs:43,306`).

**Invariants / failure behavior**
- Empty links are valid (no local evidence) and should not be API failures.
- Confidence is comparative heuristic, not calibrated probability.
- Replacement semantics prevent stale duplicates.

**Test anchors**
- Session-vs-temporal preference, stable correlation IDs, replacement+capping behavior.

---

### 4.4 Top Explanations (Hypothesis Ranking Layer)

**Purpose**
- Rank likely causes for a request anomaly using request/context signals.

**Canonical model**
- `Explanation` fields: request id, anomaly kind, rank, confidence, summary, evidence JSON (`src/stats.rs:289`).

**Write path**
- `explain_request` over `ExplanationContext` (`src/explainer.rs:11`, `src/explainer.rs:6`).
- `replace_explanations_for_request` enforces coherent latest set (`src/stats.rs:2324`).

**Read path**
- `get_explanations_for_request` and `/api/explanations` (`src/stats.rs:2267`, `src/dashboard.rs:44,314`).

**Invariants / failure behavior**
- Explanation generation failure must not affect request ingest.
- Empty explanation sets are valid output.
- Rank ordering must remain deterministic and meaningful.

**Test anchors**
- Replacement correctness and scenario-based explanation behavior.

---

### 4.5 Session Timeline (Chronological Fusion Layer)

**Purpose**
- Present unified chronological context across requests, local events, and conversation artifacts.

**Construction paths**
- `/api/timeline` endpoint builds mixed request+local event stream (`src/dashboard.rs:322`).
- `get_session_details` builds richer timeline including conversation entries (`src/stats.rs:959`, timeline assembly at `src/stats.rs:1075+`).

**Ordering/truncation behavior**
- Deterministic ordering with timestamp + tie-breakers.
- Explicit truncation flags communicate payload clipping (`truncated_sections`).
- Payload caps are enforced by `enforce_session_details_payload_cap` (`src/stats.rs:3712`).

**Invariants / failure behavior**
- Timeline must remain stable under cap/truncation.
- Unknown session path should produce explicit fallback semantics.
- Conversation scan bounds and truncation must be surfaced, not hidden.

**Test anchors**
- Session-details ordering limits, payload trimming order, and unknown-session behavior.

## 5) Cross-Domain Interaction Matrix

| Producer | Consumer | Contract | Key Risk |
|---|---|---|---|
| Request ingest | Sessions | `session_id`, timestamps, status/timing fields | Session identity ambiguity |
| Request ingest | Correlation engine | request time/session hint + recent local events | temporal skew / sparse local evidence |
| Request ingest | Explainer | anomaly features + context | confidence overstatement |
| Correlations | Graph/UI | request↔local_event edges | stale or missing replacement set |
| Explanations | Graph/UI | request↔explanation edges + rank | evidence/hypothesis drift |
| Session details | Timeline/UI | bounded merged sections + truncation flags | payload growth and client misinterpretation |

## 6) Recommended Improvement Sequencing

1. **Contract hardening (first)**
   - Formalize and version response contracts for session/correlation/explanation/timeline endpoints.
   - Add derived-artifact freshness metadata (generation timestamps/version).

2. **Identity consistency (second)**
   - Consolidate proxy/local session merge semantics into a server-side merged endpoint.
   - Reduce duplicated reconciliation logic in dashboard JS.

3. **Interpretability (third)**
   - Add provenance fields for correlation and explanation scoring.
   - Clarify confidence semantics in API docs and UI labels.

4. **Scalability/operability (fourth)**
   - Add bounded recomputation strategy for derived artifacts.
   - Increase observability around enrichment failures without blocking ingest.

## 7) Testing Strategy for Future Changes

For any change in these domains, require:
- Contract tests for affected endpoint payload shape and ordering semantics.
- Determinism tests for ranking and correlation replacement behavior.
- Session presence regression tests (proxy/local/both/none).
- Truncation/cap tests to ensure explicit signaling is preserved.

## 8) Risks and Trade-offs

- **Over-coupling risk:** If explanation logic begins to depend too tightly on correlation internals, independent diagnosis paths degrade.
- **Payload growth risk:** Session details can become expensive if timeline/conversation expansion isn’t bounded and explicit.
- **Interpretation risk:** Confidence values may be misread as probabilistic certainty unless semantics stay explicit.

## 9) Acceptance Criteria for This Analysis Deliverable

- Clear domain-by-domain mapping from data write paths to API/UI read paths.
- Explicit invariants and failure handling per domain.
- Concrete cross-domain dependency matrix.
- Sequenced recommendations that can drive implementation planning.
