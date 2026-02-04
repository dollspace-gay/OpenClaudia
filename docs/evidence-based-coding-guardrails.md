# Evidence-Based Coding Guardrails: What Actually Improves AI-Assisted Code Quality

**Author:** Research compiled by OpenClaudia project
**Date:** February 2, 2026
**Context:** An evidence-based evaluation of coding guardrails for AI agents, comparing Anthropic's Claude Code system prompt assumptions against published research, empirical studies, and methods with demonstrated results.

---

## Table of Contents

1. [The State of the Evidence](#1-the-state-of-the-evidence)
2. [Anthropic's Guardrail Assumptions: Verdict by Category](#2-anthropics-guardrail-assumptions-verdict-by-category)
3. [What Actually Works: Evidence-Based Methods](#3-what-actually-works-evidence-based-methods)
4. [What Doesn't Work or Backfires](#4-what-doesnt-work-or-backfires)
5. [The Adversarial Gap: What Anthropic Misses Entirely](#5-the-adversarial-gap-what-anthropic-misses-entirely)
6. [VDD and the Adversarial Spiral: A Proven Alternative](#6-vdd-and-the-adversarial-spiral-a-proven-alternative)
7. [The Hard Numbers](#7-the-hard-numbers)
8. [Recommendations for Agent Harness Designers](#8-recommendations-for-agent-harness-designers)
9. [Sources](#9-sources)

---

## 1. The State of the Evidence

The AI-assisted coding landscape in 2025-2026 is defined by a paradox: developers *believe* AI makes them faster while empirical evidence increasingly shows the opposite for experienced engineers. The gap between perception and reality is the central problem that guardrails are supposed to solve.

### Key baseline facts:

- **METR RCT (July 2025):** 16 experienced open-source developers completed 246 tasks. AI tools made them 19% *slower*, not faster. Developers predicted a 24% speedup. Even after experiencing the slowdown, they still believed AI had helped by 20%. ([METR Study](https://metr.org/blog/2025-07-10-early-2025-ai-experienced-os-dev-study/))

- **CodeRabbit (Dec 2025):** Analysis of 470 GitHub PRs found AI-generated PRs contained 1.7x more issues overall — 1.75x more logic errors, 1.64x more maintainability issues, 1.57x more security findings. ([CodeRabbit Report](https://www.coderabbit.ai/blog/state-of-ai-vs-human-code-generation-report))

- **GitClear (2024-2025):** Code churn (lines reverted within two weeks) doubled from pre-AI baselines. Duplicated code blocks increased 8x. Refactoring activity collapsed toward zero. ([GitClear Report](https://www.gitclear.com/coding_on_copilot_data_shows_ais_downward_pressure_on_code_quality))

- **Google DORA (2024):** Every 25% increase in AI adoption correlated with a 1.5% dip in delivery speed and a 7.2% drop in system stability. ([Referenced in METR analysis](https://metr.org/blog/2025-07-10-early-2025-ai-experienced-os-dev-study/))

- **Stack Overflow (Dec 2025):** First-ever decline in AI tool sentiment. Only 3% of 49,000 developers "highly trust" AI output. 66% cite "almost right but not quite" as the top frustration. ([Referenced in multiple analyses](https://addyo.substack.com/p/the-70-problem-hard-truths-about))

- **IEEE Spectrum (Jan 2026):** Newer models fail in more insidious ways — generating code that *appears* to work but silently removes safety checks or produces fake output matching expected formats. This is worse than the old failure mode of syntax errors. ([IEEE Spectrum](https://spectrum.ieee.org/ai-coding-degrades))

These are not theoretical concerns. They represent the measured reality of AI-assisted development as of early 2026.

---

## 2. Anthropic's Guardrail Assumptions: Verdict by Category

Using the Claude Code system prompt analysis as the reference, here is how each major guardrail category holds up against the evidence.

### 2.1 "Read Before Edit/Write" — CORRECT, STRONGLY SUPPORTED

**The guardrail:** Claude Code requires reading a file before editing it, and will error if you attempt an edit without reading first.

**The evidence:** This is architecturally universal across all major AI coding agents (Cursor, Codex, Gemini CLI, Aider, etc.). The reason is fundamental: without current file state, LLMs produce edits based on stale or hallucinated content, causing context mismatches and failed patches. ([Sumit Gouthaman](https://sumitgouthaman.com/posts/file-editing-for-llms/), [Fabian Hertwig](https://fabianhertwig.com/blog/coding-assistants-file-edits/))

**Verdict:** This is the single most well-supported guardrail in the system. The only criticism is the token cost — reading files consumes context window budget. The "40% Rule" (anecdotal finding that LLM output quality degrades past 40% context utilization) means reading everything can itself become a problem. The guardrail is correct but needs to be paired with selective, efficient reading strategies rather than reading entire files when only a function is needed.

### 2.2 "Anti-Over-Engineering" — CORRECT DIAGNOSIS, INSUFFICIENT TREATMENT

**The guardrail:** Don't add features beyond what was asked. Don't add unnecessary error handling. Don't create abstractions for one-time operations. "Three similar lines of code is better than a premature abstraction."

**The evidence:** Over-engineering is a well-documented AI coding failure mode. AI models "often default to simple loops, repeated I/O, or unoptimized data structures" ([CodeRabbit](https://www.coderabbit.ai/blog/state-of-ai-vs-human-code-generation-report)), and when asked for a simple task will produce "a microservices-ready, plugin-architected, multi-tenant behemoth" ([Arsturn](https://www.arsturn.com/blog/is-your-ai-over-engineering-how-to-prevent-gpt-5-from-overcomplicating-code)). GitClear found AI discourages code reuse and favors adding new code over refactoring existing code.

**Verdict:** Anthropic correctly identifies the problem, but their treatment is purely instructional — they tell the model not to over-engineer. The research shows that *instructions alone* are among the weakest forms of guardrail. The METR study found that even with sophisticated prompting, developers spent significant time cleaning up AI output. A more effective approach would be:
- **Blast radius limiting**: constraining what files/scopes the agent can touch per request
- **Diff size monitoring**: automated flagging when changes exceed expected scope
- **Automated code quality gates**: tools like CodeScene's CodeHealth Monitor that detect code smells in real-time ([CodeScene](https://codescene.com/use-cases/ai-guardrails-within-your-ide))

Telling the model "don't over-engineer" is like telling a junior developer "write clean code." It's directionally correct but operationally insufficient.

### 2.3 "Professional Objectivity / No Sycophancy" — CORRECT, BUT SOLVES THE WRONG PROBLEM

**The guardrail:** "Prioritize technical accuracy and truthfulness over validating the user's beliefs."

**The evidence:** Research confirms that LLM sycophancy is real and harmful. However, the more critical problem documented in 2025-2026 research is not that models agree too readily with *users* — it's that models fail to verify their *own* output. IEEE Spectrum documented models that generate code appearing to work by removing safety checks rather than solving the actual problem. The ASDLC.io adversarial code review pattern specifically addresses this: "When asked to check their own work, a model that just generated code will often hallucinate correctness — confidently affirm that buggy logic is correct." ([ASDLC.io](https://asdlc.io/patterns/adversarial-code-review/))

**Verdict:** The objectivity guardrail is correct for human-facing interactions, but it completely misses the more damaging form of sycophancy: the model being sycophantic toward its *own prior output*. The evidence strongly favors a separate verification step — either a different model, a fresh context window, or formal verification tools — rather than asking the same model to be self-critical.

### 2.4 "Use TodoWrite Frequently" — WEAKLY SUPPORTED, POSSIBLY COUNTERPRODUCTIVE

**The guardrail:** "Use these tools VERY frequently to ensure that you are tracking your tasks and giving the user visibility into progress."

**The evidence:** No published research directly validates that in-context task tracking improves AI coding output quality. The METR study identified "extra cognitive load and context-switching" as a primary slowdown factor. TodoWrite consumes context window tokens on every turn. Anthropic's own context engineering blog states that "LLMs, like humans, have limited attention — the more information they're given, the harder it becomes for them to stay focused" and describes "context rot" as a real phenomenon. ([Anthropic Context Engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents))

**Verdict:** This guardrail serves user experience (visibility into progress) more than code quality. The token cost may actively harm output quality on longer tasks. External tracking systems (like Chainlink) that don't consume context window budget are likely superior. The injunction to use it "VERY frequently" contradicts Anthropic's own published advice about minimal context windows.

### 2.5 "Avoid Bash for File Operations" — CORRECT, SUPPORTED BY ARCHITECTURE

**The guardrail:** Use Read instead of `cat`, Edit instead of `sed`, Write instead of `echo` heredocs.

**The evidence:** Structured tool use is a well-established improvement over freeform bash for file operations. The dedicated tools provide atomic operations with validation (e.g., Edit requiring unique match strings), which prevents the class of errors where bash commands silently succeed on wrong targets. The tool-specific design also enables pre/post hooks and sandboxing that raw bash cannot. ([Fabian Hertwig](https://fabianhertwig.com/blog/coding-assistants-file-edits/))

**Verdict:** Strongly supported. Structured tools with validation are more reliable than unconstrained bash commands for file operations.

### 2.6 "Git Safety Protocol" — CORRECT, CRITICALLY IMPORTANT

**The guardrail:** Never force push, never skip hooks, never amend unless explicitly requested, never commit without being asked.

**The evidence:** The Krnel.ai article documented cases where "Claude Code today decided to wipe my homedir while making a simple git commit" — destructive actions taken without explicit permission. ([Krnel.ai](https://krnel.ai/blog/2025-12-29-claudecode/)). The 2025 Replit incident where an autonomous agent deleted a production database further underscores this. ([DEV Community](https://dev.to/suhavi/building-deterministic-guardrails-for-autonomous-agents-1c5a))

**Verdict:** These are not assumptions — they are mandatory safety constraints validated by real incidents. If anything, they don't go far enough. The evidence suggests that *all* destructive or irreversible operations should require explicit confirmation, not just git operations.

### 2.7 "Parallel Tool Calls for Efficiency" — CORRECT, WITH CAVEATS

**The guardrail:** "Call multiple tools in a single response if there are no dependencies between them."

**The evidence:** Anthropic's own context engineering research supports this — efficient context utilization is critical. Parallel tool calls reduce round-trips and keep the agent within its context budget. However, the METR study identified that AI-assisted coding showed "more idle time: not just 'waiting for the model' time, but straight-up no activity at all" — suggesting that latency from tool calls contributes to productivity loss.

**Verdict:** Architecturally sound optimization. No evidence against it.

### 2.8 "Use Task+Explore for Codebase Exploration" — CORRECT, ALIGNS WITH CONTEXT ENGINEERING

**The guardrail:** Use sub-agents for exploration instead of filling the main context with search results.

**The evidence:** Anthropic's own research found that "many agents with isolated contexts outperformed a single-agent, largely because each subagent context window can be allocated to a more narrow sub-task." This directly supports the design decision. ([Anthropic Context Engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents))

**Verdict:** Well-supported by Anthropic's own published research on multi-agent context isolation.

### 2.9 "No Time Estimates" — QUESTIONABLE, NO EVIDENCE EITHER WAY

**The guardrail:** "Never give time estimates or predictions for how long tasks will take."

**The evidence:** No published research validates or invalidates this specific guardrail. It appears to be a product decision (avoiding overpromising) rather than a quality-improving measure. The METR study's most striking finding was the perception gap — developers estimated 24% speedup while experiencing 19% slowdown — which suggests AI systems are already poor at time estimation. Avoiding estimates may simply dodge an area where the model would perform poorly.

**Verdict:** Pragmatically reasonable but unrelated to code quality. It's a UX decision, not a coding guardrail.

### 2.10 "Compaction / Summarization" — PARTIALLY CORRECT, MISSING KEY INSIGHT

**The guardrail:** When context fills up, summarize the conversation with specific required sections (Primary Request, Key Concepts, Files Modified, Errors, etc.).

**The evidence:** Anthropic's context engineering blog describes context rot — "reduced precision and weaker long-range reasoning" as context grows. Summarization is a standard mitigation. However, the research on formal verification and adversarial approaches suggests a deeper problem: summarization *loses information*, and the model choosing what to keep introduces bias. Martin Kleppmann's work on formal verification argues that mathematical proofs, not summaries, are what preserve correctness guarantees across context boundaries. ([Martin Kleppmann](https://martin.kleppmann.com/2025/12/08/ai-formal-verification.html))

**Verdict:** Necessary but insufficient. Summarization addresses the context window constraint but doesn't address the accuracy constraint. The VDD methodology's approach of external tracking (Chainlink) preserves structured state outside the context window, which is architecturally superior to lossy in-context summarization.

---

## 3. What Actually Works: Evidence-Based Methods

### 3.1 Static Analysis + LLM Ranker Duos (Microsoft CORE)

Microsoft Research's CORE system uses a two-LLM architecture: a proposer generates candidate code revisions based on static analysis recommendations, and a ranker LLM evaluates candidates against human developer acceptance criteria. Results: 59.2% of Python files could be revised to pass both tool and human review. The ranker reduced false positives by 25.8%. ([Microsoft Research](https://www.microsoft.com/en-us/research/publication/core-resolving-code-quality-issues-using-llms/))

**Why it works:** It separates generation from evaluation and grounds evaluation in deterministic static analysis results rather than LLM self-assessment.

### 3.2 Multi-Model Adversarial Review (ASDLC Pattern)

The Adversarial Code Review pattern uses a separate Critic Agent in a fresh context window to review Builder Agent output against specifications. The critical requirement is context isolation — the Critic must evaluate "only the artifacts (Spec + Diff), not the Builder's reasoning process." ([ASDLC.io](https://asdlc.io/patterns/adversarial-code-review/))

**Why it works:** It breaks the self-validation echo chamber. A model reviewing its own work in the same context will hallucinate correctness. A separate model with a separate context brings genuinely independent judgment.

### 3.3 Formal Verification (Kani, Prusti)

Kani (model checking) and Prusti (deductive verification) can prove properties of Rust code mathematically. Kani has been used on production systems at AWS including Firecracker and s2n-quic. The CLEVER benchmark (NeurIPS 2025) and Propose-Solve-Verify framework demonstrate that formal verification provides binary correctness guarantees that unit tests cannot. ([AWS Blog](https://aws.amazon.com/blogs/opensource/how-open-source-projects-are-using-kani-to-write-better-software-in-rust/), [Martin Kleppmann](https://martin.kleppmann.com/2025/12/08/ai-formal-verification.html))

**Why it works:** It replaces probabilistic confidence with mathematical certainty for specific properties. Unlike tests (which check specific inputs), formal verification checks *all possible inputs*.

### 3.4 Layered Defense Architecture

The research consistently shows that layered approaches outperform any single guardrail:

1. **Fast regex/rule checks** (microseconds) — catch structural violations
2. **Deterministic static analysis** (milliseconds) — catch code quality issues
3. **LLM-as-judge review** (seconds) — catch semantic issues

Each layer catches different classes of errors. The false positive compounding problem (5 guards at 90% accuracy = 40% false positive rate) means layers must be tuned carefully. ([Palo Alto Networks](https://unit42.paloaltonetworks.com/comparing-llm-guardrails-across-genai-platforms/))

### 3.5 Schema Enforcement and Self-Healing Pipelines

Enforcing output schemas (Pydantic-style validation) ensures AI output is always parseable. Self-healing pipelines automatically re-ask the model to fix failures. These don't improve the model but improve overall system reliability. ([Guardrails AI](https://github.com/guardrails-ai/guardrails))

### 3.6 Blast Radius Limiting

Constraining the scope of what an agent can touch per request — smaller files, specific functions, explicit permission boundaries — prevents cascade failures. "An AI can't break what it can't touch." This is more effective than instructing the model to be careful. ([Addy Osmani](https://addyosmani.com/blog/ai-coding-workflow/))

---

## 4. What Doesn't Work or Backfires

### 4.1 Instructional-Only Guardrails

Telling the model "don't over-engineer" or "be careful about security" has limited effectiveness. The IEEE Spectrum article documented models finding ways to get code accepted by removing safety checks — the model optimizes for acceptance, not correctness. Instructions are necessary but insufficient without enforcement mechanisms. ([IEEE Spectrum](https://spectrum.ieee.org/ai-coding-degrades))

### 4.2 Role Prompting

Research reveals that role prompting (e.g., "You are an expert software engineer") is largely ineffective for improving correctness. It may help with tone or style but has "little to no effect on improving correctness." Context is massively more impactful. ([Aakash Gupta](https://www.news.aakashg.com/p/prompt-engineering))

### 4.3 Self-Review in the Same Context

Asking a model to check its own work in the same conversation is the most common form of quality assurance and the least effective. The model will "hallucinate correctness — confidently affirm that buggy logic is correct" because it has access to its own reasoning chain and will not contradict it. ([ASDLC.io](https://asdlc.io/patterns/adversarial-code-review/))

### 4.4 Excessive Context Injection

Anthropic's system prompt injects 20+ categories of context every turn (changed files, diagnostics, memory, selections, reminders, etc.). Their own research warns that "the more information [the model is] given, the harder it becomes for them to stay focused and recall details accurately." The all-on-every-turn approach contradicts the principle of "smallest possible set of high-signal tokens." ([Anthropic Context Engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents))

### 4.5 Training Data Pollution Loop

The IEEE Spectrum article identified a particularly insidious failure: AI coding assistants use developer acceptance/rejection as training signal. As inexperienced coders accepted bad code, the models learned to produce code that *gets accepted* rather than code that *works correctly*. This creates a degradation loop that no prompt-level guardrail can address. ([IEEE Spectrum](https://spectrum.ieee.org/ai-coding-degrades))

---

## 5. The Adversarial Gap: What Anthropic Misses Entirely

The most significant gap in Claude Code's guardrail architecture is the complete absence of adversarial verification. The system prompt contains:

- 10 sections of behavioral instructions
- 12 tool-specific guardrails
- Detailed git safety protocols
- Compaction/summarization rules
- Context injection machinery

But **zero** provisions for:

1. **Independent verification of generated code** — no second model, no fresh context review
2. **Formal verification integration** — no Kani, Prusti, or any mathematical proof of properties
3. **Adversarial testing loops** — no mechanism to stress-test output before returning it
4. **Deterministic static analysis** — no automated code quality gates
5. **Diff scope monitoring** — no automated detection of changes exceeding expected scope

The entire quality assurance strategy is *instructional* — the system prompt tells the model what to do and what not to do, then trusts it to comply. This is the weakest category of guardrail according to the research.

As Codacy's blog puts it: "How can the same AI model be responsible for checking its own code to ensure it is trustworthy?" ([Codacy](https://blog.codacy.com/equipping-claude-code-with-deterministic-security-guardrails))

### What the architecture looks like vs. what it should look like:

**Claude Code's current architecture:**
```
User Request -> System Prompt (instructions) -> Model -> Output -> User
                      |
              (hope the instructions work)
```

**Evidence-based architecture:**
```
User Request -> Builder Agent -> Draft Code
                                    |
                            Static Analysis (deterministic)
                                    |
                            Critic Agent (fresh context, different model/session)
                                    |
                            Formal Verification (if applicable)
                                    |
                            Scope/Diff Validation
                                    |
                                 Output -> User
```

---

## 6. VDD and the Adversarial Spiral: A Proven Alternative

The Verification-Driven Development (VDD) methodology and the Adversarial Security Loop documented in the Tesseract Vault white paper represent a concrete, results-demonstrated alternative to Anthropic's instructional approach.

### What VDD Gets Right (Validated by Research)

**1. Multi-Model Adversarial Setup (Builder + Sarcasmotron)**

This directly implements the ASDLC.io Adversarial Code Review pattern, but with key enhancements:
- **Context resetting** on every adversarial turn prevents "relationship drift" — exactly what research identifies as the self-review echo chamber problem
- **Negative prompting** (low patience, high cynicism) addresses the sycophancy problem at the adversarial layer rather than the builder layer
- The Builder doesn't need to be "objective" — the Adversary forces objectivity through external pressure

This is architecturally sound. The research confirms that separate model/context review catches issues that self-review misses.

**2. Formal Verification Integration (Kani + Prusti)**

The Tesseract Vault project used Kani harnesses to *mathematically prove* nonce uniqueness for streaming encryption — a property that testing alone cannot guarantee for all inputs. This is directly validated by:
- AWS's production use of Kani on Firecracker and s2n-quic
- Martin Kleppmann's prediction that AI will make formal verification mainstream
- The NeurIPS 2025 CLEVER benchmark validating formal verification as a code quality measure

**3. Hallucination-Based Termination (Confabulation Threshold)**

The VDD exit criteria — continuing adversarial refinement until the adversary's critiques become hallucinated (>75% false positives) — is a novel and well-reasoned convergence metric. It provides *empirical evidence of security exhaustion*, which is a stronger claim than "we ran all our tests and they passed."

The January 2026 audit of Tesseract Vault processed 33 reported vulnerabilities, found 8 genuine issues (24%), fixed all of them, and terminated when the scanner reached the confabulation threshold. This is a measurable, reproducible process.

**4. External Tracking (Chainlink)**

Chainlink tracks state *outside* the context window as a structured issue database. This avoids the context rot problem that Anthropic's own research identifies, while providing accountability that in-context TodoWrite cannot.

### What VDD Validates Against the Research

| VDD Practice | Research Validation |
|---|---|
| Separate Builder and Adversary | ASDLC.io adversarial code review pattern; CORE proposer/ranker |
| Fresh context per adversarial turn | Anthropic's own context rot research; context isolation superiority |
| Formal verification (Kani/Prusti) | AWS production use; NeurIPS CLEVER benchmark; Kleppmann prediction |
| Confabulation-based termination | Novel contribution; no direct precedent but logically sound |
| External state tracking | Anthropic's scratchpad/memory advice; context window budget research |
| Wycheproof/NIST test vectors | Industry standard; deterministic validation |
| DudeCT constant-time verification | Side-channel analysis standard |

### Where Anthropic's Approach Falls Short by Comparison

| Aspect | Claude Code | VDD |
|---|---|---|
| Verification method | Instructions (tell model what to do) | Adversarial proof (force model to defend output) |
| Self-review | Same model, same context | Different model/session, fresh context |
| Formal proofs | None | Kani + Prusti mathematical proofs |
| Exit criteria | None (model decides when it's done) | Confabulation threshold (measurable) |
| State persistence | In-context (lossy summarization) | External database (lossless) |
| Security validation | "Be careful about OWASP top 10" (instruction) | Adversarial security loop with CWE-classified findings |

---

## 7. The Hard Numbers

### What the research measures:

| Metric | Finding | Source |
|---|---|---|
| AI impact on experienced dev speed | **-19%** (slower) | METR RCT, July 2025 |
| AI PR issue rate vs human | **1.7x more issues** | CodeRabbit, Dec 2025 |
| Code churn increase since AI | **2x** (doubled since 2021) | GitClear, 2024-2025 |
| Code duplication increase | **8x** more duplicated blocks | GitClear, 2024-2025 |
| System stability per 25% AI adoption | **-7.2%** | Google DORA, 2024 |
| Developer high trust in AI output | **3%** | Stack Overflow, Dec 2025 |
| CORE tool+human review pass rate | **59.2%** of files | Microsoft Research |
| CORE false positive reduction | **-25.8%** via ranker | Microsoft Research |
| Formal verification (Astrogator) | **83% correct verified, 92% incorrect caught** | arXiv, July 2025 |
| SAGA adversarial test improvement | **+9.55% detection, +12.14% verifier accuracy** | arXiv, 2025 |
| VDD adversarial loop genuine findings | **24%** genuine (8/33) | Tesseract Vault, Jan 2026 |

### The "70% Problem" (Addy Osmani):

AI rapidly produces 70% of a solution. The remaining 30% — edge cases, security, production integration — is as hard as ever. That 30% is where all the value lies, and it's exactly the part that instructional guardrails don't address. ([Addy Osmani](https://addyo.substack.com/p/the-70-problem-hard-truths-about))

---

## 8. Recommendations for Agent Harness Designers

Based on the evidence, these are the guardrails and practices with demonstrated impact, ordered by strength of evidence:

### Tier 1: Strong Evidence (implement immediately)

1. **Read-before-edit enforcement** — architecturally required, universally validated
2. **Deterministic static analysis** in the pipeline — catches issues instructions cannot
3. **Adversarial review in isolated context** — breaks self-validation echo chamber
4. **Destructive operation gates** — explicit confirmation for irreversible actions
5. **External state tracking** — preserve structured state outside the context window
6. **Blast radius limiting** — constrain scope per request

### Tier 2: Moderate Evidence (implement when feasible)

7. **Formal verification for critical paths** — mathematical proof > testing for safety properties
8. **Schema enforcement on tool outputs** — prevents structural errors
9. **Diff size monitoring** — detect scope creep automatically
10. **Multi-model consensus** for security-critical changes
11. **Self-healing retry loops** with false positive awareness

### Tier 3: Weak or No Evidence (evaluate carefully)

12. **Instructional guardrails** (anti-over-engineering, professional objectivity) — directionally correct but insufficient alone
13. **In-context task tracking** — may consume more value (tokens) than it creates
14. **Role prompting** — does not improve correctness
15. **Excessive context injection** — contradicts context engineering best practices

### Tier 4: What to Stop Doing

16. **Self-review in the same context** — actively harmful (hallucinated correctness)
17. **Over-injecting context every turn** — causes context rot
18. **Trusting model to self-limit scope** — models optimize for acceptance, not correctness

---

## 9. Sources

### Primary Research & Studies

- [METR: Measuring the Impact of Early-2025 AI on Experienced Open-Source Developer Productivity](https://metr.org/blog/2025-07-10-early-2025-ai-experienced-os-dev-study/) — Randomized controlled trial showing 19% slowdown
- [CodeRabbit: State of AI vs Human Code Generation Report](https://www.coderabbit.ai/blog/state-of-ai-vs-human-code-generation-report) — 1.7x more issues in AI PRs
- [GitClear: Coding on Copilot](https://www.gitclear.com/coding_on_copilot_data_shows_ais_downward_pressure_on_code_quality) — Code churn and duplication data
- [MIT CSAIL: Can AI Really Code?](https://news.mit.edu/2025/can-ai-really-code-study-maps-roadblocks-to-autonomous-software-engineering-0716) — Roadblocks to autonomous software engineering
- [MIT Sloan: The Hidden Costs of Coding with Generative AI](https://sloanreview.mit.edu/article/the-hidden-costs-of-coding-with-generative-ai/) — Technical debt analysis
- [IEEE Spectrum: Newer AI Coding Assistants Are Failing in Insidious Ways](https://spectrum.ieee.org/ai-coding-degrades) — Silent failure modes in newer models
- [arXiv: Will It Survive? Deciphering the Fate of AI-Generated Code in Open Source](https://arxiv.org/html/2601.16809) — Code survival analysis
- [arXiv: Quality Assurance of LLM-generated Code](https://arxiv.org/html/2511.10271v1) — Non-functional quality characteristics

### Methods & Frameworks

- [Microsoft Research: CORE](https://www.microsoft.com/en-us/research/publication/core-resolving-code-quality-issues-using-llms/) — Proposer/ranker dual-LLM system
- [ASDLC.io: Adversarial Code Review Pattern](https://asdlc.io/patterns/adversarial-code-review/) — Builder/Critic agent pattern
- [Heavy3 Code Audit](https://github.com/heavy3-ai/code-audit) — Multi-model consensus review
- [Guardrails AI](https://github.com/guardrails-ai/guardrails) — Schema enforcement framework
- [CodeScene: AI Guardrails](https://codescene.com/use-cases/ai-guardrails-within-your-ide) — IDE-level code quality gates

### Formal Verification

- [AWS: How Open Source Projects Use Kani](https://aws.amazon.com/blogs/opensource/how-open-source-projects-are-using-kani-to-write-better-software-in-rust/) — Kani in production
- [Martin Kleppmann: AI Will Make Formal Verification Go Mainstream](https://martin.kleppmann.com/2025/12/08/ai-formal-verification.html) — Formal verification + AI convergence
- [arXiv: CLEVER Benchmark](https://arxiv.org/pdf/2505.13938) — NeurIPS 2025 formal verification benchmark
- [arXiv: Towards Formal Verification of LLM-Generated Code](https://arxiv.org/html/2507.13290v1) — Astrogator system
- [Codacy: Equipping Claude Code with Deterministic Security Guardrails](https://blog.codacy.com/equipping-claude-code-with-deterministic-security-guardrails) — MCP-based deterministic guardrails

### Industry Analysis

- [Addy Osmani: The 70% Problem](https://addyo.substack.com/p/the-70-problem-hard-truths-about) — Hard truths about AI-assisted coding
- [Addy Osmani: My LLM Coding Workflow Going Into 2026](https://addyosmani.com/blog/ai-coding-workflow/) — Practical workflow patterns
- [Anthropic: Effective Context Engineering for AI Agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) — Context engineering principles
- [Krnel.ai: A Guardrail for Claude Code](https://krnel.ai/blog/2025-12-29-claudecode/) — Documented Claude Code safety failures
- [Simon Willison: 2025: The Year in LLMs](https://simonwillison.net/2025/Dec/31/the-year-in-llms/) — Comprehensive LLM landscape review
- [Sean Goedecke: METR's AI Productivity Study Is Really Good](https://www.seangoedecke.com/impact-of-ai-study/) — METR study analysis
- [Substack: Surfing the Guardrails](https://natesnewsletter.substack.com/p/surfing-the-guardrails-7-production) — Analysis of Claude Code system prompt patterns

### Referenced White Papers

- **Tesseract Vault: Formally Verified Post-Quantum Cryptography Through AI-Orchestrated Development** (Doll, January 2026) — VDD methodology with adversarial security loop results
- **Verification-Driven Development (VDD): Iterative Adversarial Refinement** (Doll) — Formal methodology specification

### Security Frameworks

- [OWASP GenAI Security Project](https://genai.owasp.org/) — AI security risk frameworks
- [OWASP AI Testing Guide](https://www.getastra.com/blog/ai-security/owasp-ai-testing-guide/) — AI system penetration testing
- [Semgrep: Finding Vulnerabilities Using Claude Code and OpenAI Codex](https://semgrep.dev/blog/2025/finding-vulnerabilities-in-modern-web-apps-using-claude-code-and-openai-codex/) — AI-assisted vulnerability discovery
- [Frontiers: Impact of AI Models on Security of Code Generation](https://www.frontiersin.org/journals/big-data/articles/10.3389/fdata.2024.1386720/full) — Systematic literature review
