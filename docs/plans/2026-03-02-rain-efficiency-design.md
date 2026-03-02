# Rain Efficiency Design

## Problem

Rain takes 79 seconds and 22 LLM roundtrips to answer "hey rain." Three factors compound to make every interaction expensive:

1. **No thinking budget control.** ZeroClaw sends no `thinkingConfig`, so Gemini defaults to maximum reasoning on every call — including triage, greetings, and simple tool use.
2. **Thinking text replayed in history.** `extract_response` captures full chain-of-thought text into `raw_model_parts`. This gets replayed on every subsequent API call, growing context linearly with each iteration.
3. **Unbounded context accumulation.** The channel orchestrator uses `run_tool_call_loop`, which sends the entire accumulated history on every iteration. A planner/executor architecture already exists in `planner.rs` but is only wired into the CLI path.

These multiply together: each of 22 iterations sends growing history filled with reasoning text at maximum thinking level.

## Lever 1: Strip Thinking Text from History

### Location

`extract_response` in `gemini.rs` (~line 402)

### Current behavior

`all_parts` captures every part from the model response, including `thought: true` parts containing raw chain-of-thought text. These are stored in `raw_model_parts` on the assistant message and replayed verbatim on every subsequent API call.

### Change

When building `all_parts`, keep only parts that serve a structural purpose:

| Part type | Keep? | Why |
|-----------|-------|-----|
| `thought_signature` present | Yes | Gemini requires signatures for conversation continuity |
| `function_call` present | Yes | Tool use history |
| Non-thought text (`thought` is false/None, `text` is Some) | Yes | The actual answer |
| `thought: true`, no signature | **Drop** | Raw reasoning — large, never referenced again |

### Impact

~40-60% token reduction per iteration. Compounds across roundtrips since every subsequent call carries less history.

## Lever 2: thinkingConfig Driven by Model Matrix

### Location

`GenerationConfig` struct in `gemini.rs` (~line 304) and request construction (~line 1488)

### Current behavior

`GenerationConfig` contains only `temperature` and `maxOutputTokens`. No `thinkingConfig` is sent. Gemini defaults to maximum reasoning on every call.

### Change

1. Add `thinkingConfig` as a peer field to `generationConfig` in the request body (per Gemini API spec — it sits at the top level, not nested inside `generationConfig`).
2. Thread the model route hint through to the provider so it can set the appropriate thinking level.

**Tier mapping:**

| Route hint | thinkingLevel | Use case |
|-----------|--------------|----------|
| `simple` | `minimal` | Greetings, acknowledgments |
| `medium` | `low` | Standard tool use |
| `complex` | `medium` | Multi-step reasoning |
| `reasoning` | `high` | Deep analysis |
| triage | `minimal` | Channel triage gate |
| planner | `low` | Planning call — structured output, not deep reasoning |
| heartbeat | `minimal` | Periodic check-ins |

3. The existing `query_classification` system already maps messages to tiers, which map to model routes. The route hint now also determines thinking level — no new classification logic needed.

### Impact

~50-70% token reduction on simple and medium calls. Triage calls (every incoming message) become significantly cheaper.

## Lever 3: Wire Planner into Channel Orchestrator

### Location

`process_channel_message` in `orchestrator.rs` (~line 1121)

### Current behavior

Slack messages go through `run_tool_call_loop` — a single long-running loop that sends the full accumulated history on every iteration. The planner/executor architecture (`plan_then_execute` in `planner.rs`) exists but is only called from the CLI path in `agent.rs`.

### How the planner works

1. **Planning call**: No tools, just system prompt + user message. Returns either a `Passthrough` (simple message, respond directly) or a JSON action plan with grouped steps.
2. **Executor calls**: Each action gets a fresh 2-message history (system prompt + action instruction). Actions in the same group run in parallel via `futures::join_all`.

### Change

1. In `process_channel_message`, after triage passes, route through `plan_then_execute` instead of `run_tool_call_loop`.
2. Pass session history context to the planning call so the planner understands conversation continuity.
3. Collect executor results back into the channel response.

**What stays the same:**
- Triage gate still runs first, still uses triage model
- Query classification still scores and routes to model tier
- Conversation history still persisted per channel/thread
- `Passthrough` for simple messages — zero overhead for greetings

### Impact

Eliminates the core scaling problem. Cost becomes **additive** (planning call + N independent action calls) instead of **multiplicative** (N iterations x growing context). For a 22-roundtrip task, each action starts fresh instead of carrying accumulated history from all previous iterations.

## Combined Effect

| Scenario | Current cost (tokens) | After all three levers |
|----------|----------------------|----------------------|
| Triage call | High thinking + full context | Minimal thinking, same context |
| Simple greeting | 22 iterations x growing context x max thinking | Passthrough: 1 call, minimal thinking |
| Multi-step task (e.g. standup) | N iterations, each with full accumulated history + thinking text | 1 cheap planning call + N fresh executor calls, each with appropriate thinking level |

The three levers are independent and stack:
- **thinkingConfig** reduces token cost per call
- **Stripping thinking text** reduces context size per iteration
- **Planner routing** eliminates context accumulation across iterations

## Files to Modify

All changes are in the zeroclaw codebase (`/Users/ra/Programming/zeroclaw`):

| File | Change |
|------|--------|
| `src/providers/gemini.rs` | Strip thinking parts from `all_parts` in `extract_response`; add `thinkingConfig` to request body; thread route hint to provider |
| `src/channels/orchestrator.rs` | Replace `run_tool_call_loop` with `plan_then_execute` in `process_channel_message` |
| `src/agent/planner.rs` | May need minor adjustments to accept channel context |

Configuration changes in spore-pm:

| File | Change |
|------|--------|
| `zeroclaw.toml` | Add planner model route if not already present; verify query classification thresholds |

## Risks

- **Planner misclassification.** A complex request classified as `Passthrough` would get a single response with no tools. Mitigation: the planner already has threshold tuning (`skip_threshold = 0.3`, `activate_threshold = 0.5`), and we can adjust after observing behavior.
- **Executor context too slim.** Fresh 2-message history per action means each executor lacks prior action results. Mitigation: the planner already handles this by structuring independent actions into groups — dependent actions go in separate sequential groups.
- **Thinking level too low.** Minimal thinking on triage might miss nuanced intent. Mitigation: triage is a binary relevance check, not a reasoning task. Monitor false negatives.
