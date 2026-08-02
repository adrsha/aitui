# Prompt Architecture Analysis

## My System Prompt (opencode main thread)

The prompt governing this conversation includes the following structural components:

1. **Identity**: "You are opencode, an interactive CLI tool that helps users with software engineering tasks."

2. **Tool definitions**: JSON Schema for each tool (bash, edit, glob, grep, question, read, skill, task, todowrite, webfetch, websearch, write) with parameter schemas and usage rules.

3. **Response rules**:
   - Use tools to complete tasks, not text output
   - GitHub-flavored markdown for formatting
   - Be concise, direct, no emoji unless asked
   - Minimize output tokens
   - No preamble/postamble or code explanation unless asked
   - Answer in <4 lines unless user asks for detail

4. **Code style rules**:
   - No comments added to code
   - Follow existing conventions, libraries, frameworks
   - Security best practices

5. **Proactiveness rules**:
   - Do the right thing when asked, don't surprise with extra actions
   - Don't add explanation after working on files

6. **Tool usage policy**:
   - Batch independent tool calls
   - Use Task tool for multi-step work
   - Prefer specialized tools over bash for file operations
   - Chain sequential commands with `&&`

7. **Git rules**:
   - Never commit/amend/push unless explicitly asked
   - Inspect before committing
   - No force-push, no interactive -i

8. **AGENTS.md injection** (caveman mode):
   - "Respond terse like smart caveman"
   - "Drop: articles, filler, pleasantries, hedging"
   - "Fragments OK. Short synonyms. Technical terms exact."
   - Switch levels: /caveman lite|full|ultra|wenyan

9. **Skill system**: Available skills can be loaded with descriptions.

10. **Environment info**: Working directory, platform, date.

---

## Application Sub-Agent Prompts (aitui)

The application defines agents at two levels: (A) opencode's own sub-agents (investigator/builder/reviewer) and (B) the coding assistant agent that the application itself manages.

### A. opencode Sub-Agent Prompts

#### cavecrew-investigator (~56 lines)
- **Header**: Read-only code locator. Returns file:line table.
- **Mode directive**: "Caveman-ultra. Drop articles/filler/hedging."
- **Job**: "Locate. Report. Stop. Never edit, never propose fix."
- **Output format**: `<path:line> — \`<symbol>\` — <≤6 word note>` with grouping headers.
- **Tools**: Grep, Glob, Read, Bash (limited).
- **Refusals**: Template responses for out-of-scope requests.
- **Example**: Complete I/O example.

#### cavecrew-builder (~46 lines)
- **Header**: Surgical 1-2 file edit.
- **Mode directive**: "Caveman-ultra. Drop articles/filler."
- **Scope**: "1 file ideal. 2 OK. 3+ → refuse."
- **Workflow**: Read → Edit → Re-Read → Return receipt.
- **Output format**: `<path:line-range> — <change ≤10 words>. verified: <OK|mismatch>.`
- **Refusals**: Terminal-line templates for too-big/needs-confirm/ambiguous/regressed.

#### cavecrew-reviewer (~47 lines)
- **Header**: Diff/branch/file reviewer.
- **Mode directive**: "Caveman-ultra. Findings only."
- **Severity table**: 🔴 bug / 🟡 risk / 🔵 nit / ❓ question with emoji + tier.
- **Output format**: `path:line: <emoji> <severity>: <problem>. <fix>.`
- **Boundaries**: Review only what's in front, no "while we're here".

### B. Application's Own Agent Prompts

#### Main Agent System Prompt (~236 lines in `tools.rs`)
- **Identity**: "You are an agentic coding assistant running INSIDE a terminal app."
- **Environment**: Working directory, file system access.
- **Tool format**: `<tool>` XML-style tag specification.
- **Five tool categories**: file_management, shell, web, interaction, workflow.
- **Communication**: "Lead with the outcome. Be readable."
- **Task execution**: Software engineering focus, todo checklist, one step at a time.
- **Safety**: Reversibility, blast radius, act-don't-ask policy.
- **Multi-agent**: Parallel child agents with barrier waits.
- **Batch calls**: Efficient within step.

#### Child Agent (Subtask) Prompt (~19 lines in `subtask.rs`)
- **Identity**: "You are a focused child agent at delegation depth N."
- **Scope**: "Complete only the delegated task below."
- **Tools**: Read-only project tools, web research, shell, workflow.
- **Constraints**: "Do not mutate files, ask the user, or manage the parent's todos."
- **Budgets**: Soft tool-round limit, absolute request limit, duration cap.

#### Access Judge Prompt (~33 lines in `access.rs`)
- **Identity**: "You are an access-control classifier for a coding assistant's tool calls."
- **Decision categories**: allow / deny / ask.
- **Principle**: "Be conservative. When in doubt, answer 'ask'."
- **Output**: JSON array only, no prose.

#### Autonomous Loop Prompt (~4 lines in `effects.rs`)
- **Directive**: "AUTONOMOUS LOOP MODE is active."
- **Goal + Stop Criteria**: User-provided parameters.
- **Instruction**: "Make concrete, verifiable progress. Do it, don't describe it."

#### Follow-up Suggestions Prompt (~2 lines in `suggestions.rs`)
- **Instruction**: "Generate exactly three concise follow-up prompts..."
- **Output**: JSON array of three strings.

#### Skills Wrapper Prompt (~3 lines in `effects.rs`)
- **Directive**: "Active skills are mandatory response-shaping instructions."
- **Application**: "Apply every active skill to every answer."

---

## Structural Comparison

| Feature | My Prompt (opencode) | Sub-Agent Prompts (aitui) |
|---|---|---|
| **Identity** | Explicit ("You are opencode...") | Explicit per role (investigator/builder/agent) |
| **Voice / mode** | Caveman (compressed), with switch levels | Caveman-ultra for most; full prose for main agent |
| **Tool definitions** | Full JSON Schema per tool (30+ params each) | Simple allow-lists ("Grep, Glob, Read, Bash") |
| **Output format** | Implicit (GitHub markdown, no preamble) | Structured format with examples (path:line pattern, emoji severity) |
| **Scope enforcement** | Implicit ("do the task") | Explicit refusals with template strings |
| **Error handling** | Implicit | Explicit error formats (terminal lines, mismatch receipts) |
| **Examples** | None | I/O example for investigator, diff receipt for builder |
| **Chaining** | Not specified | Explicit chaining patterns (locate→fix→verify, parallel scout) |
| **Boundaries** | Some ("Only commit when asked") | Extensive (formatting nits skip, no "while we're here", no drive-by refactors) |
| **Auto-clarity** | Mentioned for security warnings | Explicit in every sub-agent |

## Key Differences

1. **Precision of output format**: My prompt says "be concise" and "GitHub-flavored markdown". The sub-agents have exact output templates with line-by-line structure, examples, and fallback tokens like `No match.` or `No issues.` This makes sub-agent outputs parseable by the main thread.

2. **Refusal system**: I have implicit refusals ("I cannot do that"). Sub-agents have terminal-line refusal patterns (`too-big.`, `ambiguous.`, `needs-confirm.`) that the calling code can pattern-match against.

3. **Tool access restriction**: I get all tools. Sub-agents have explicitly limited tool sets — `cavecrew-builder` has no `Bash` at all, `cavecrew-investigator` has only read tools.

4. **Context compression**: The entire skill system exists to reduce token consumption of sub-agent output. My prompt doesn't address this — it just says "minimize output tokens."

5. **Safety prompts**: The application has a dedicated access judge prompt (allow/deny/ask) that runs BEFORE any tool execution. My prompt has safety rules embedded in the main instruction but no separate classifier.

6. **Mode switching**: My prompt has `/caveman lite|full|ultra|wenyan` levels. The sub-agents have less granular control — mostly "caveman-ultra" fixed.

7. **Chaining documentation**: The cavecrew skill explicitly documents how to chain sub-agents (locate→fix→verify, parallel scout). My prompt has no equivalent orchestration pattern documentation.

8. **Schema-driven vs instruction-driven**: Tool definitions in my prompt are formal JSON Schema. The application's agent tool definitions are written as prose instructions with `<tool>` tag examples.
