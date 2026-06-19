# Production Audit Problems

Audit date: 2026-06-19

This document records product-impacting problems found by reading the current
codebase, docs, and tests. It is intentionally focused on risks that affect
production correctness, security, feature completeness, or claims made by the
binary and README.

## Executive Summary

OpenClaudia has a broad test suite and many real subsystems, but the largest
production risk is architectural drift between entrypoints. Default TUI,
legacy REPL, ACP, one-shot print mode, proxy mode, subagents, and intercepted
tool execution each assemble requests or execute tools through separate paths.
That makes it easy for new gates such as enterprise policy, grounding, hooks,
permissions, token accounting, and ledger observations to land in one surface
but not another.

The most important correction is to stop adding per-entrypoint fixes and
instead centralize request policy and tool dispatch into shared services that
all binary modes must use.

## Findings

### P0: Enterprise Policy Is Not Enforced Across All Entrypoints

Problem:
The `policy` config is implemented as a production control plane, but it is not
consistently applied to every way the binary can call providers or tools.

Evidence:
- `src/services/policy.rs:19` documents the shipped policy surface, including
  proxy request gates for model allowlists and token caps.
- `src/proxy.rs:1513` enforces model policy for proxy chat requests, and
  `src/proxy.rs:1523` enforces token policy after context preparation and
  compaction.
- `src/pipeline.rs:2180` enforces `PolicyEnforcer::check_and_record_tool` for
  the default TUI tool loop when a policy enforcer and session id are present.
- `src/main.rs:441` through `src/main.rs:508` loads config, resolves model,
  endpoint, and headers for the default TUI without checking model allowlist or
  token caps before launching the direct provider path.
- `src/cli/print_mode.rs:236` through `src/cli/print_mode.rs:292` builds and
  sends a direct one-shot provider request without checking model allowlist,
  request token cap, or session token cap.
- `src/acp.rs:1459` through `src/acp.rs:1468` builds an ACP
  `ChatCompletionRequest`, and `src/acp.rs:1529` sends it directly without
  enterprise policy checks.
- `src/acp.rs:1948` through `src/acp.rs:1981` gates ACP tool execution through
  hooks and permissions, then dispatches, but has no `PolicyEnforcer` tool-cap
  check.
- `src/cli/chat_repl.rs:1854` through `src/cli/chat_repl.rs:1864` and
  `src/cli/chat_repl.rs:2670` through `src/cli/chat_repl.rs:2680` show legacy
  REPL tool execution checking permission only before dispatch.
- `src/subagent.rs:1447` through `src/subagent.rs:1454` sets up a permission
  manager for subagent tool execution, but no enterprise policy enforcer.

Impact:
Operators can configure policy expecting it to apply to OpenClaudia, but a user
can bypass parts of it by choosing another binary mode. Model allowlists and
token caps are strongest through proxy mode. Tool caps are currently strongest
through the default TUI. ACP, legacy REPL, print mode, and subagents lag behind.

Recommendation:
Introduce one shared request gate, for example `ProviderRequestPolicy`, that
checks model allowlist and token caps before any provider request leaves the
process. Introduce one shared tool gate, for example `ToolExecutionPolicy`,
that is invoked by every tool execution path before hooks or dispatch. Make
TUI, legacy REPL, ACP, proxy, subagents, and intercepted tool execution use
those shared gates.

### P0: Tool Execution Is Duplicated Across Independent Dispatch Paths

Problem:
Tool execution is implemented in several places with subtly different gate
ordering and side effects.

Evidence:
- `src/pipeline.rs:2140` through `src/pipeline.rs:2215` implements the default
  TUI tool loop with permission checks, policy tool caps, TUI events, execution,
  and ledger observation.
- `src/cli/chat_repl.rs:1882` through `src/cli/chat_repl.rs:1905` implements
  Gemini legacy REPL tool dispatch and audit setup.
- `src/cli/chat_repl.rs:2696` through `src/cli/chat_repl.rs:2725` implements
  Anthropic/OpenAI legacy REPL tool dispatch and audit setup.
- `src/acp.rs:1937` through `src/acp.rs:1995` implements ACP tool execution
  with its own hook/permission/dispatch sequence.
- `src/tool_intercept.rs:1044` through `src/tool_intercept.rs:1067` executes
  intercepted tools locally with ledger observation but without the same policy
  chain as the TUI.

Impact:
Every security, ledger, policy, hook, or UX fix must be ported by hand to
multiple execution loops. That is already producing parity gaps in enterprise
policy and makes future production hardening expensive and error-prone.

Recommendation:
Create a single `ToolExecutor` service that owns the gate order:
parse arguments, plan-mode restriction, enterprise tool cap, permission check,
pre-hook, dispatch, post-hook, audit, ledger observation, and result shaping.
Each entrypoint should supply UI adapters for prompts/events, not reimplement
the execution semantics.

### P1: The Grounded Agent Loop Is Partial, Not the Core Execution Model

Problem:
The Reality Ledger, grounding hierarchy, typed decisions, and final gate exist,
but provider output is still mostly natural-language/tool-call driven. The
typed `AgentDecision` validator is not the central executor for production
turns.

Evidence:
- `src/decision.rs:20` through `src/decision.rs:40` defines typed
  `AgentDecision` variants.
- `src/decision.rs:57` through `src/decision.rs:128` validates decisions
  against authoritative ledger evidence.
- `src/grounded_loop.rs:353` through `src/grounded_loop.rs:377` injects a
  grounding system message into provider requests. This is prompt guidance, not
  a typed decision protocol.
- `src/grounded_loop.rs:386` through `src/grounded_loop.rs:423` validates final
  text after the model has already produced it.
- Production paths such as ACP still stream provider output directly and then
  call the final gate on the final text at `src/acp.rs:1574`.

Impact:
The low-drift architecture is a meaningful improvement, but it is not yet the
single source of truth for actions. The model can still drift during ordinary
tool-call turns, and final validation is a post-hoc rejection rather than a
typed policy gate before action.

Recommendation:
Move from:
messages plus grounding prompt -> provider -> tool calls/final text

to:
ledger packet -> provider -> typed decision -> policy/evidence validation ->
executor -> ledger.

Start by requiring typed decisions for edits and commands in one path, then
expand that path to TUI, ACP, REPL, and subagents.

### P1: Final Grounding Relies On Prompt Obedience And Post-Hoc Rejection

Problem:
The final gate requires cited observation ids and verification observations,
but the model is only prompted to cite them. If the model gives a useful final
answer without citations, the app rejects it after the turn.

Evidence:
- `src/final_gate.rs:38` through `src/final_gate.rs:150` requires
  authoritative evidence, verifier observations, and backing for test/file
  claims.
- `src/final_gate.rs:158` through `src/final_gate.rs:173` extracts cited
  observation ids from final answer text.
- `src/grounded_loop.rs:414` through `src/grounded_loop.rs:423` records a
  policy denial when final text fails the gate.

Impact:
This is safer than accepting ungrounded answers, but it can degrade UX and does
not prevent wasted provider turns. It also pushes correctness onto prompt
compliance rather than protocol shape.

Recommendation:
Make final output a structured decision with explicit `evidence` and
`verification` fields, then render a human answer after validation. Keep text
citation extraction only as compatibility fallback.

### P1: There Is No Single Binary Capability Matrix

Problem:
The test suite is broad, but there is no single matrix that proves every
documented binary command and major feature works through every relevant
entrypoint.

Evidence:
- `README.md:188` through `README.md:220` documents the binary command surface:
  default TUI, legacy REPL, `--print`, `init`, `auth`, `start`, `acp`, `loop`,
  `config`, and `doctor`.
- `README.md:15` advertises "30+ Agentic Tools" across bash, file ops, LSP,
  web search, notebooks, task tracking, plan mode, worktrees, cron scheduling,
  and MCP resources.
- `tests/cli_exit_status_e2e.rs` covers many CLI exit and documentation
  invariants, but enterprise policy, grounding, and tool parity are not tested
  as a matrix over TUI, REPL, ACP, proxy, print, and subagents.

Impact:
The project can have many tests and still miss entrypoint drift. Production
readiness requires proving that each advertised command either works or fails
with an intentional documented limitation.

Recommendation:
Add a generated or hand-maintained capability matrix test suite. Rows should be
features/claims. Columns should be binary modes. Each cell should assert
"works", "unsupported with documented error", or "not applicable".

### P1: ACP Model Selection Is Static And Rejects Non-Advertised Models

Problem:
ACP model configuration accepts only models returned by
`acp_model_option_ids`, which is based on current model plus static fallback
catalogs. This conflicts with the goal of allowing all provider models and with
providers that support dynamic model listing.

Evidence:
- `src/acp.rs:626` through `src/acp.rs:648` builds ACP model options from the
  current model and static catalog entries.
- `src/acp.rs:750` through `src/acp.rs:756` rejects any ACP model value not in
  those advertised options.
- `src/proxy.rs:515` through `src/proxy.rs:525` can fetch dynamic upstream
  model lists when an adapter supports model listing, but ACP does not use that
  mechanism.
- `README.md:236` through `README.md:237` documents model listing/switching in
  the TUI, but the ACP config path is more restrictive.

Impact:
New provider models can be usable through direct model override but rejected by
ACP configuration until the static list is updated or the model is already the
current model. This is especially likely to regress the "latest models" goal.

Recommendation:
Treat static catalogs as suggestions, not allowlists. ACP should either support
free-form model input or hydrate options from the same provider model-listing
path as proxy mode. Enterprise model allowlists should be the only hard model
restriction.

### P2: TUI App Construction Reads Config As A Side Effect

Problem:
`App::new` loads config from disk to construct its policy enforcer instead of
receiving the already-loaded config from startup.

Evidence:
- `src/main.rs:441` loads config and applies target/model startup overrides.
- `src/main.rs:595` constructs `tui::app::App::new(model, &config.proxy.target)`.
- `src/tui/app.rs:844` through `src/tui/app.rs:874` has `App::new` call
  `crate::config::load_config()` again to create `PolicyEnforcer`.

Impact:
The app constructor performs filesystem I/O and can diverge from the config
object that startup already loaded and validated. It also makes tests and
embedding harder because constructing an app depends on ambient project config.

Recommendation:
Pass an `Arc<PolicyEnforcer>` or policy snapshot through `TuiLaunchOptions`.
Constructors should not reload global config.

### P2: Legacy `/compact` Drops Custom Instructions

Problem:
The legacy REPL accepts `/compact <instructions>` syntactically but drops the
free-text instructions.

Evidence:
- `src/cli/repl/slash.rs:3105` through `src/cli/repl/slash.rs:3119` pins this
  as an intentional divergence: the argument is currently ignored and only
  `SlashCommandResult::Compact` is returned.

Impact:
Users can ask compaction to preserve specific context and get no error, while
the instruction is silently ignored.

Recommendation:
Change the command result to carry optional custom instructions, plumb that
through compaction, and update tests to assert that the argument is preserved.

### P2: Management And Slash Command Completeness Is Split By UI Mode

Problem:
The default TUI intentionally exposes a smaller command set than the legacy
REPL, while README and help surfaces must be kept very clear about which mode
owns which command.

Evidence:
- `README.md:222` through `README.md:224` says the default TUI exposes a
  focused slash-command set and the legacy REPL has additional commands.
- `src/slash_commands.rs:271` through `src/slash_commands.rs:275` says TUI
  slash sections intentionally exclude legacy REPL-only commands and management
  overlay stubs until `tui::app::App` implements them.

Impact:
This is documented, so it is not a direct bug. It is still a production
completeness gap if the product goal is feature parity across the default user
experience.

Recommendation:
Keep the TUI command registry as the source of truth for default UI help.
Either implement the missing management/plugin/config commands in the TUI or
move them out of primary product claims.

### P2: Session Token Policy Documentation Does Not Match The Current Proxy Calculation

Problem:
The policy comment describes `max_session_tokens` as a sum of `max_tokens`
budgets, while proxy enforcement projects current actual usage plus estimated
input plus output budget.

Evidence:
- `src/services/policy.rs:86` documents `max_session_tokens` as "sum of
  `max_tokens` budgets".
- `src/proxy.rs:1453` through `src/proxy.rs:1460` computes projected session
  policy tokens from cumulative total, estimated input, and output budget.
- `src/proxy.rs:1484` through `src/proxy.rs:1496` checks that projected session
  total against the session cap.

Impact:
The implementation may be the better behavior, but the operator-facing
semantics are ambiguous. A production policy must be predictable.

Recommendation:
Update docs/config comments to define the exact enforced formula, or change the
code to match the documented "sum of max_tokens budgets" model.

### P2: Coordinator And Background Memory Agent Work Is Explicitly Incomplete

Problem:
Some advanced agent features exist as design-stage or partial infrastructure,
not complete production behavior.

Evidence:
- `README.md:28` describes the subagent system but also says coordinator
  infrastructure is experimental and not wired into the default TUI.
- `README.md:196` documents `--coordinator --tui-mode` as a legacy REPL mode.
- `docs/designs/507-coordinator.md:99` through
  `docs/designs/507-coordinator.md:107` describes a phased plan where the first
  coordinator phase is infrastructure/no behavioral change.
- `docs/designs/508-memdir.md:13` through `docs/designs/508-memdir.md:19`
  lists missing memory-related background systems: autoDream, MagicDocs,
  SessionMemory, PromptSuggestion, and MEMORY.md entrypoint.

Impact:
These are not hidden bugs because several are documented as incomplete. They
are still important production-readiness gaps if the goal is Claude Code-like
agent orchestration and memory behavior.

Recommendation:
Keep these features labeled experimental until default TUI wiring, lifecycle
tests, and failure-mode tests exist. Avoid using broad parity language for
design-only systems.

### P2: Static Model Catalogs Must Not Become Capability Gates

Problem:
The codebase contains both dynamic model listing and static fallback model
catalogs. Static lists are useful defaults, but they become production risks
when any UI or protocol treats them as exhaustive.

Evidence:
- `src/proxy.rs:515` through `src/proxy.rs:525` attempts dynamic provider model
  listing where supported, then falls back to static catalogs.
- `src/acp.rs:626` through `src/acp.rs:648` uses static catalogs for ACP model
  options.
- `src/acp.rs:750` through `src/acp.rs:756` turns those options into a hard
  validation list.

Impact:
Provider model launches can require code changes even when the provider would
accept the model name. This directly conflicts with the goal of allowing all
models from connected providers.

Recommendation:
Keep static catalogs for defaults, examples, and offline fallback only. Use
provider model listing when available, and permit explicit model ids unless
enterprise policy denies them.

## Already Mitigated Or Not A Current Finding

- Web search is currently documented as free DuckDuckGo/Bing browser scraping
  in default builds (`README.md:17`). That aligns with the recent direction to
  remove paid search cruft.
- Cron scheduling is documented as metadata for external schedulers
  (`README.md:31`), so the lack of an internal daemon is not itself a doc
  overclaim.
- MCP support is documented at the level of browsing/reading resources
  (`README.md:37`). Unsupported transport experiments should stay out of user
  claims unless fully wired and tested.

## Recommended Fix Order

1. Build shared request and tool policy gates, then wire every binary mode
   through them.
2. Replace duplicated tool dispatch loops with a single `ToolExecutor`.
3. Convert one production path to typed `AgentDecision` execution and expand
   from there.
4. Add the binary capability matrix so future audit work has a fixed
   completion target.
5. Make model discovery dynamic/free-form everywhere static catalogs are
   currently treated as exhaustive.
6. Reconcile README/help text with the features that remain intentionally
   legacy-only or experimental.
