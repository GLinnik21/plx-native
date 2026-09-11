export const meta = {
  name: 'swarm-gate',
  description: 'Decompose a task, build it with a bounded Sonnet swarm in isolated worktrees, then gate the merged result behind one Opus review per wave',
  whenToUse: 'A multi-file task large enough to split across parallel workers, whose result must pass the project\'s real blocking checks AND an independent review before anyone calls it done. Not for a one-file edit.',
  phases: [
    { title: 'Prepare',   detail: 'read the task and project rules; record the git baseline and the blocking checks', model: 'sonnet' },
    { title: 'Measure',   detail: 'single-purpose probes: digest the user\'s uncommitted work, list what each branch really touched', model: 'haiku' },
    { title: 'Decompose', detail: 'packages with owned files, declared dependencies and dependency waves', model: 'sonnet' },
    { title: 'Contract',  detail: 'freeze the shared interfaces before anything runs in parallel', model: 'sonnet' },
    { title: 'Implement', detail: 'bounded waves of Sonnet workers, one git worktree each', model: 'sonnet' },
    { title: 'Weave',     detail: 'merge each wave onto the session branch BEFORE the next wave is cut from it', model: 'sonnet' },
    { title: 'Integrate', detail: 'merge sequentially into the working branch, commit, run every blocking check', model: 'sonnet' },
    { title: 'Review',    detail: 'exactly one Opus per wave, read-only, over the whole task rather than the last diff', model: 'opus' },
    { title: 'Fix',       detail: 'findings grouped so no two concurrent fixers share a file; one Sonnet per group', model: 'sonnet' },
    { title: 'Reapply',   detail: 'serial lane for fix branches that turned out to touch the same file after all', model: 'sonnet' },
  ],
}

// ---------------------------------------------------------------------------
// swarm-gate v2
//
//   Prepare -> Measure(baseline) -> Decompose -> [Contract]
//     -> (Implement wave -> Weave)* -> Integrate -> Measure -> Review(1 Opus)
//     -> [ Fix(disjoint groups) -> Measure(real files) -> Integrate
//          -> [Reapply serial] -> Measure -> Review ] * maxFixRounds
//     -> Report
//
// ROUND SEMANTICS, fixed and unambiguous (maxFixRounds = 2):
//     implement -> review #0 -> fix wave #1 -> review #1 -> fix wave #2 -> review #2 -> STOP
//   maxFixRounds counts FIX WAVES. Opus runs at most maxFixRounds + 1 times.
//   `fix_waves_used` and `reviews_run` are reported separately so the number is
//   never inferred from the other.
//
// WHAT IS ENFORCED BY CODE, NOT BY ASKING A MODEL NICELY
//   * stage order is `await` order;
//   * maxWorkers is a chunk size, so a wave physically cannot exceed it;
//   * maxFixRounds is a `for` bound; maxPackages and maxReviewFindings bound the
//     blast radius of a broken decomposer or a runaway reviewer;
//   * a null agent result is a HARD FAILURE, retried at most maxAgentRetries
//     times and then recorded — never an implicit success;
//   * the reviewer runs in a THROWAWAY worktree, so "the reviewer does not edit
//     code" is structural rather than instructed;
//   * PASS is a conjunction computed here, and one of its terms is a byte
//     comparison the reporting agent never sees (below).
//
// WHY EVERY WAVE IS WOVEN BACK IN BEFORE THE NEXT IS CUT (v3 — a fix for a real
// failure, not a refinement). A worker's worktree is cut from the SESSION BRANCH at
// the moment it is spawned. In v2 nothing merged between waves, so a package in wave
// N+1 was cut from a branch that did NOT contain wave N's commits: `depends_on`
// ordered the waves in TIME but not in CODE, and a dependent package could not open
// the very work it depended on — it got a prose paragraph describing it instead.
// Observed on phase 7 of this repository's UI restructure: `p3` was told to extend
// `screens/detail/mod.rs`, a file `p2` had created in a DIFFERENT worktree, so in
// p3's tree it did not exist. It failed, retried, and burned 731K tokens looking for
// it. The decomposer had half-sensed the hazard and chained p2 -> p3 -> p4 on that one
// shared file, which also destroyed the parallelism the swarm exists for.
// So `Weave` merges each wave onto the session branch and commits, and only then is
// the next wave cut. A wave is an INTEGRATION POINT, not just a schedule slot.
//
// WHY WORKERS CUT THEIR OWN WORKTREE INSTEAD OF USING `isolation: 'worktree'`
// (v4 — again a fix for an observed failure, not a preference). The harness option
// does not reliably cut from the branch this run is building on. Measured on the
// second phase-7 attempt: all six workers were handed worktrees at `e61baca5`, a
// commit predating the entire restructure — no `screens/`, no `stores/`, no `app/`
// — while the session branch was 22 commits ahead. The discriminating evidence is
// clean: the ONE stage without `isolation: 'worktree'` (Contract) ran in the session
// worktree, saw the right tree, and committed real work in the same run.
// So every stage that needs a private tree now creates it itself from an EXPLICIT
// commit and VERIFIES `git rev-parse HEAD` before touching anything. The base comes
// from the probe's measured `HEAD_SHA` (§ the trust boundary) rather than from any
// agent's claim about where it left the branch.
//
// THE ONE HONEST LIMITATION, STATED RATHER THAN PAPERED OVER
//   A workflow script has no filesystem and no shell. It cannot run `git diff`
//   itself. So the user's uncommitted work is protected by SEPARATING THE
//   MEASURER FROM THE INTERESTED PARTY: a single-purpose probe agent runs three
//   fixed commands and returns their stdout; this script compares the digests
//   and decides. The integrator — the one agent with a motive to report success
//   — neither takes the measurement nor sees it. A digest that does not validate
//   as 64 hex characters is `unknown`, and `unknown` is not PASS. Fail-closed.
// ---------------------------------------------------------------------------

// ============================== helpers ====================================

function clampInt(v, dflt, lo, hi) {
  const n = Number(v)
  if (!Number.isFinite(n)) return dflt
  return Math.max(lo, Math.min(hi, Math.trunc(n)))
}

// Run `items` through `make` at most `limit` at a time. A chunk is a barrier,
// which is deliberate: it is what makes maxWorkers an enforced ceiling rather
// than a hope, and it keeps the run resumable at a chunk boundary.
async function inWaves(items, limit, make) {
  const out = []
  for (let i = 0; i < items.length; i += limit) {
    const chunk = items.slice(i, i + limit)
    if (items.length > limit) log(`  slice ${1 + ((i / limit) | 0)}: ${chunk.length} worker(s) of ${items.length} queued`)
    const got = await parallel(chunk.map((it, k) => () => make(it, i + k)))
    for (let k = 0; k < chunk.length; k++) out.push({ item: chunk[k], result: got[k] })
  }
  return out
}

// Kahn-style layering over `depends_on`. A cycle is a decomposition defect, so
// it is LOGGED and the remainder runs one-at-a-time — silently flattening a
// cycle into a parallel wave is how two workers end up editing one contract.
function toWaves(pkgs) {
  const byId = new Map(pkgs.map(p => [p.id, p]))
  const done = new Set(); const waves = []
  let left = pkgs.slice()
  while (left.length) {
    const ready = left.filter(p => (p.depends_on || []).every(d => done.has(d) || !byId.has(d)))
    if (!ready.length) {
      log(`! dependency cycle among ${left.map(p => p.id).join(', ')} — running them ONE AT A TIME`)
      for (const p of left) waves.push([p])
      return { waves, cycle: true }
    }
    for (const p of ready) done.add(p.id)
    waves.push(ready)
    left = left.filter(p => !ready.includes(p))
  }
  return { waves, cycle: false }
}

// Union-find over a key -> files mapping. Two entries sharing a file end up in
// one bucket. An entry with NO files is treated as overlapping every other
// unknown one: assuming disjointness from missing data is the single mistake
// that produces a lost edit.
function clusterByFiles(entries, filesOfEntry) {
  if (!entries.length) return []
  const parent = entries.map((_, i) => i)
  const find = i => { while (parent[i] !== i) { parent[i] = parent[parent[i]]; i = parent[i] } return i }
  const union = (a, b) => { const ra = find(a), rb = find(b); if (ra !== rb) parent[ra] = rb }
  const owner = new Map()
  let unknown = -1
  entries.forEach((e, i) => {
    const files = (filesOfEntry(e) || []).filter(Boolean)
    if (!files.length) { if (unknown < 0) unknown = i; else union(unknown, i); return }
    for (const f of files) { if (owner.has(f)) union(owner.get(f), i); else owner.set(f, i) }
  })
  const buckets = new Map()
  entries.forEach((e, i) => {
    const r = find(i)
    if (!buckets.has(r)) buckets.set(r, [])
    buckets.get(r).push(e)
  })
  return [...buckets.values()]
}

function filesOf(list) {
  const s = new Set()
  for (const f of list) for (const p of (f.files || [])) if (p) s.add(p)
  return [...s]
}

const PRIORITY_ORDER = { blocker: 0, high: 1, medium: 2, low: 3 }

// ============================== schemas =====================================

const PREP = {
  type: 'object', additionalProperties: false,
  required: ['branch', 'is_default_branch', 'baseline_sha', 'dirty_files', 'required_checks', 'acceptance_criteria'],
  properties: {
    branch: { type: 'string' },
    is_default_branch: { type: 'boolean', description: 'true if the checkout sits on the repo default branch (main/master)' },
    baseline_sha: { type: 'string' },
    dirty_files: { type: 'array', items: { type: 'string' } },
    required_checks: {
      type: 'array', minItems: 1,
      items: {
        type: 'object', additionalProperties: false,
        required: ['name', 'command', 'blocking'],
        properties: { name: { type: 'string' }, command: { type: 'string' }, blocking: { type: 'boolean' } },
      },
    },
    acceptance_criteria: { type: 'array', items: { type: 'string' }, minItems: 1 },
    project_rules: { type: 'array', items: { type: 'string' } },
    spec_summary: { type: 'string' },
    blockers: { type: 'array', items: { type: 'string' } },
  },
}

const TOUCHED = {
  type: 'object', additionalProperties: false,
  required: ['branches'],
  properties: {
    branches: {
      type: 'array',
      items: {
        type: 'object', additionalProperties: false,
        required: ['branch', 'files'],
        properties: {
          branch: { type: 'string' },
          files: { type: 'array', items: { type: 'string' }, description: 'verbatim stdout lines of git diff --name-only' },
          error: { type: 'string' },
        },
      },
    },
  },
}

const DECOMP = {
  type: 'object', additionalProperties: false,
  required: ['packages'],
  properties: {
    shared_interfaces: {
      type: 'array',
      items: {
        type: 'object', additionalProperties: false,
        required: ['name', 'files', 'spec'],
        properties: { name: { type: 'string' }, files: { type: 'array', items: { type: 'string' } }, spec: { type: 'string' } },
      },
    },
    packages: {
      type: 'array', minItems: 1, maxItems: 24,
      items: {
        type: 'object', additionalProperties: false,
        required: ['id', 'title', 'files_owned', 'depends_on', 'prompt', 'acceptance'],
        properties: {
          id: { type: 'string' }, title: { type: 'string' },
          files_owned: { type: 'array', items: { type: 'string' }, minItems: 1 },
          depends_on: { type: 'array', items: { type: 'string' } },
          prompt: { type: 'string' }, acceptance: { type: 'string' },
        },
      },
    },
    shared_files: { type: 'array', items: { type: 'string' } },
    risks: { type: 'array', items: { type: 'string' } },
  },
}

const WORK = {
  type: 'object', additionalProperties: false,
  required: ['id', 'status', 'branch', 'worktree', 'files_changed', 'summary'],
  properties: {
    id: { type: 'string' },
    status: { type: 'string', enum: ['done', 'partial', 'blocked'] },
    branch: { type: 'string' }, worktree: { type: 'string' }, head_sha: { type: 'string' },
    files_changed: { type: 'array', items: { type: 'string' } },
    summary: { type: 'string' },
    shared_file_deltas: { type: 'array', items: { type: 'string' } },
    open_problems: { type: 'array', items: { type: 'string' } },
  },
}

const INTEG = {
  type: 'object', additionalProperties: false,
  required: ['merged', 'conflicts', 'checks', 'all_blocking_passed', 'head_sha', 'diff_stat'],
  properties: {
    merged: { type: 'array', items: { type: 'string' } },
    failed_to_merge: { type: 'array', items: { type: 'string' } },
    conflicts: { type: 'array', items: { type: 'string' } },
    checks: {
      type: 'array',
      items: {
        type: 'object', additionalProperties: false,
        required: ['name', 'command', 'passed'],
        properties: { name: { type: 'string' }, command: { type: 'string' }, passed: { type: 'boolean' }, blocking: { type: 'boolean' }, output_tail: { type: 'string' } },
      },
    },
    all_blocking_passed: { type: 'boolean' },
    head_sha: { type: 'string' }, diff_stat: { type: 'string' },
    notes: { type: 'array', items: { type: 'string' } },
  },
}

const REVIEW = {
  type: 'object', additionalProperties: false,
  required: ['verdict', 'summary', 'findings'],
  properties: {
    verdict: { type: 'string', enum: ['PASS', 'FIX', 'REPLAN', 'BLOCKED'] },
    replan_reason: { type: 'string', description: 'required when verdict is REPLAN: what about the DECOMPOSITION or the approach is wrong' },
    summary: { type: 'string' },
    findings: {
      type: 'array', maxItems: 40,
      items: {
        type: 'object', additionalProperties: false,
        required: ['key', 'priority', 'title', 'files', 'explanation', 'how_to_verify'],
        properties: {
          key: { type: 'string', description: 'stable kebab-case slug; RE-USE the same key when re-reporting a finding from an earlier wave' },
          priority: { type: 'string', enum: ['blocker', 'high', 'medium', 'low'] },
          title: { type: 'string' },
          files: { type: 'array', items: { type: 'string' } },
          line: { type: 'integer' },
          explanation: { type: 'string' }, how_to_verify: { type: 'string' },
        },
      },
    },
    closed_previous: { type: 'array', items: { type: 'string' } },
    still_open_previous: { type: 'array', items: { type: 'string' } },
    verified_good: { type: 'array', items: { type: 'string' } },
  },
}

const FIXED = {
  type: 'object', additionalProperties: false,
  required: ['group', 'status', 'branch', 'worktree', 'addressed', 'files_changed'],
  properties: {
    group: { type: 'string' },
    status: { type: 'string', enum: ['done', 'partial', 'blocked'] },
    branch: { type: 'string' }, worktree: { type: 'string' }, head_sha: { type: 'string' },
    addressed: { type: 'array', items: { type: 'string' } },
    declined: { type: 'array', items: { type: 'string' } },
    files_changed: { type: 'array', items: { type: 'string' } },
    open_problems: { type: 'array', items: { type: 'string' } },
  },
}

// ============================== arguments ===================================

const A = (args && typeof args === 'object' && !Array.isArray(args)) ? args : { task: typeof args === 'string' ? args : '' }
const TASK = String(A.task || '').trim()
const REPO = String(A.repo || '').trim()                     // the checkout, ABSOLUTE — pinned into every prompt (v5)
const MAX_WORKERS = clampInt(A.maxWorkers, 4, 1, 8)
const MAX_FIX_ROUNDS = clampInt(A.maxFixRounds, 2, 0, 5)      // counts FIX WAVES
const MAX_PACKAGES = clampInt(A.maxPackages, 8, 1, 24)
const MAX_FINDINGS = clampInt(A.maxReviewFindings, 30, 1, 40)
const MAX_RETRIES = clampInt(A.maxAgentRetries, 1, 0, 2)      // retries BEYOND the first attempt
const LANE_CHECKS = A.laneChecks === true

const hard_failures = []
const reviews = []
const integrations = []
const dirty_checks = []

if (!TASK) {
  log('! no `task` argument — nothing to do')
  return { ok: false, verdict: 'BLOCKED', reason: 'swarm-gate needs args.task: a task description or a path to a specification file.', fix_waves_used: 0, reviews_run: 0 }
}
if (!REPO.startsWith('/')) {
  log('! no absolute `repo` argument — refusing to let each agent guess which checkout it is in')
  return { ok: false, verdict: 'BLOCKED', reason: 'swarm-gate needs args.repo: the ABSOLUTE path of the checkout to work in. Run 3 (wf_a9647e0c-35d) showed why: with a correct cwd, the Haiku probe still prefixed `cd <primary checkout>` on its own and measured a different branch\'s HEAD, so every worker base would have been wrong.', fix_waves_used: 0, reviews_run: 0 }
}
// One line every agent sees first. It names a path, never the task, so the probe's trust
// boundary is intact; and it is the ONLY answer to "which checkout" any agent is allowed to have.
const REPO_LINE = `REPOSITORY CHECKOUT: ${REPO}\nEvery command runs there (use the absolute path, or \`git -C ${REPO}\`). It is a linked git worktree; the primary checkout and every other worktree are OTHER PEOPLE'S WORK — never cd into them, never read or measure them.`

log(`swarm-gate v5 in ${REPO}: <=${MAX_WORKERS} concurrent, <=${MAX_FIX_ROUNDS} fix wave(s) => <=${MAX_FIX_ROUNDS + 1} Opus review(s), <=${MAX_PACKAGES} packages, <=${MAX_FINDINGS} findings/wave, ${MAX_RETRIES} retry, lane checks ${LANE_CHECKS ? 'ON' : 'OFF'}`)

// A null result means the agent was skipped or died after the runtime's own
// retries. Retrying it here is bounded and explicit, and an exhausted retry is
// recorded rather than smoothed over.
async function tryAgent(prompt, opts, what, retries) {
  const max = retries === undefined ? MAX_RETRIES : retries
  const pinned = `${REPO_LINE}\n\n${prompt}`
  for (let attempt = 0; attempt <= max; attempt++) {
    const r = await agent(pinned, attempt ? { ...opts, label: `${opts.label} (retry ${attempt})` } : opts)
    if (r) return r
    if (attempt < max) log(`! ${what} returned nothing — retry ${attempt + 1}/${max}`)
  }
  hard_failures.push(`${what}: no result after ${max + 1} attempt(s)`)
  return null
}

// ======================= THE TRUST BOUNDARY ================================
// Everything below this line is a MEASUREMENT, and it is deliberately kept away
// from every agent with a stake in the answer.
//
//   * the probe is told NOTHING — not the task, not the specification, not the
//     acceptance criteria, not that a PASS depends on what it returns;
//   * it runs four fixed commands and replies in a four-line KEY=VALUE protocol
//     with no prose, so the parser on this side is a regex rather than a model;
//   * THIS SCRIPT parses and compares. No agent is asked whether the user's work
//     survived, and no agent's schema contains a field in which to claim it —
//     the integrator, the one agent that wants to report success, has no
//     `preserved_dirty_files` to set, because the field does not exist;
//   * a malformed line, a missing key, one key answered twice with different
//     values, an ERROR value, or a dead probe all yield `unknown`, and `unknown`
//     is not PASS. Fail-closed;
//   * the probe gets Haiku and ONE retry, hardcoded and not configurable upward.
//     It is a shell probe: if it cannot read four digests in two attempts it
//     will not on the fifth, and a retry loop around `git diff | shasum` is how
//     a measurement turns into a token fire.
//
// KNOWN GAP, stated rather than papered over: command 3 uses
// `--exclude-standard`, so GITIGNORED files are not digested. That is
// deliberate — this repository's ignored set includes multi-gigabyte build
// trees, and hashing them every wave would cost more than the run. Gitignored
// private files are protected by the workers' instructions and the outbound
// guard, NOT by this measurement, and this report says so rather than implying
// a coverage it does not have.
const PROBE_RETRIES = 1

const PROBE_PROTOCOL = [
  'You are a measurement probe. You have exactly one job, no context, and no opinion about the result.',
  '',
  'Run these four commands EXACTLY as written — the directory is part of the command; do not substitute another path, and do not cd anywhere first:',
  `  1. cd ${REPO} && git diff --binary | shasum -a 256 | cut -d" " -f1`,
  `  2. cd ${REPO} && git diff --cached --binary | shasum -a 256 | cut -d" " -f1`,
  `  3. cd ${REPO} && git ls-files --others --exclude-standard | LC_ALL=C sort | while IFS= read -r f; do printf "%s\\n" "$f"; shasum -a 256 "$f"; done | shasum -a 256 | cut -d" " -f1`,
  `  4. cd ${REPO} && git rev-parse HEAD`,
  '',
  'Then reply with EXACTLY these four lines and NOTHING else — no prose, no preamble, no code fence, no commentary about what you saw:',
  '',
  'DIRTY_UNSTAGED_SHA=<stdout of command 1>',
  'DIRTY_STAGED_SHA=<stdout of command 2>',
  'UNTRACKED_SHA=<stdout of command 3>',
  'HEAD_SHA=<stdout of command 4>',
  '',
  'Change nothing. Create nothing. Read no other file. Do not form a view about this repository.',
  'If a value surprises you, report it UNCHANGED: a surprising digest is precisely the signal the caller is asking for, and "correcting" it destroys the only thing this probe exists for.',
  'If a command fails, emit its key with the literal value ERROR. Never invent a digest.',
].join('\n')

// A regex, not a judgement. Anything it cannot read cleanly is `null`, which
// the caller treats as unknown.
function parseProbe(text) {
  if (typeof text !== 'string') return null
  const WANT = { DIRTY_UNSTAGED_SHA: 64, DIRTY_STAGED_SHA: 64, UNTRACKED_SHA: 64, HEAD_SHA: 0 }
  const out = {}
  for (const raw of text.split('\n')) {
    const m = /^\s*([A-Z_]+)\s*=\s*([0-9a-fA-F]+|ERROR)\s*$/.exec(raw)
    if (!m) continue
    const key = m[1]
    if (!(key in WANT)) continue
    const val = m[2].toLowerCase()
    if (key in out && out[key] !== val) return null   // one key, two answers: unusable
    out[key] = val
  }
  for (const key of Object.keys(WANT)) {
    const v = out[key]
    if (!v || v === 'error') return null
    const ok = WANT[key] === 64 ? /^[0-9a-f]{64}$/.test(v) : /^([0-9a-f]{40}|[0-9a-f]{64})$/.test(v)
    if (!ok) return null
  }
  return out
}

async function probeDirty(when) {
  const text = await tryAgent(
    PROBE_PROTOCOL,
    { label: `probe:${when}`, phase: 'Measure', model: 'haiku' },
    `probe (${when})`,
    PROBE_RETRIES,
  )
  const parsed = parseProbe(text)
  if (!parsed) log(`! probe ${when}: unusable output — this run cannot prove your uncommitted work was preserved, and will not claim to`)
  return parsed
}

// The workflow decides. The probe only measures.
function compareDirty(base, now, when) {
  if (!base) return { when, ok: null, reason: 'the baseline probe was unusable' }
  if (!now) return { when, ok: null, reason: 'this wave\'s probe was unusable' }
  const KEYS = ['DIRTY_UNSTAGED_SHA', 'DIRTY_STAGED_SHA', 'UNTRACKED_SHA']
  const moved = KEYS.filter(k => base[k] !== now[k])
  return { when, ok: moved.length === 0, moved, head_measured: now.HEAD_SHA }
}

// The worktree recipe every private-tree stage uses. `base` is a measured sha, never
// a claimed one; `slug` makes the path and branch unique per lane.
function ownWorktree(base, slug) {
  const path = `/tmp/sg-${String(base).slice(0, 8)}-${slug}`
  const branch = `sg/${String(base).slice(0, 8)}/${slug}`
  return [
    '## Make your own worktree, and VERIFY it before you touch anything',
    'This run does not rely on the harness for isolation, because it has been observed handing workers a tree three weeks stale — six of them at once, every dependency missing. So cut your own from an exact commit:',
    '```sh',
    `git -C ${REPO} worktree add -b ${branch} ${path} ${base}`,
    `cd ${path}`,
    'git rev-parse HEAD    # MUST print ' + base,
    '```',
    `If that sha is not \`${base}\`, or the worktree cannot be created, STOP and report \`blocked\` saying so. Do not work in the shared checkout and do not proceed from a base you did not verify: everything downstream assumes your commits sit on ${String(base).slice(0, 8)}.`,
    `Report \`branch\` as \`${branch}\` and \`worktree\` as \`${path}\`.`,
  ].join('\n')
}

// ============================== 1. Prepare ==================================

phase('Prepare')
const prep = await tryAgent(
  [
    'You are the PREPARE stage of a `swarm-gate` run. You write no product code, modify no file, and run NO build or test command — a dedicated integration stage owns those, and a baseline build here buys nothing the first integration will not establish anyway.',
    '', '## The task', TASK, '',
    'If the task names a file path, READ that file — it is the specification.',
    '', '## Do this',
    '1. Read the project rules that apply (AGENTS.md / CLAUDE.md / the nearest module guide). Extract only the rules a worker on THIS task could break.',
    '2. Record the git baseline: current branch, `git rev-parse HEAD`, and `git status --porcelain` — every already-dirty path is user work that must survive the run untouched.',
    '3. Say whether the checkout is on the repository DEFAULT branch. This decides whether the run may proceed at all: swarm-gate commits its integration, and committing a swarm\'s merge onto trunk is forbidden here.',
    '4. Name the REQUIRED CHECKS — the exact commands that decide whether this task is done, taken from the project\'s own documented gate rather than invented. Mark each `blocking` or not; at least one must be blocking. If the task text already names the gate commands, use THOSE verbatim and do not substitute your own.',
    '5. Write the acceptance criteria as a checkable list.',
    '',
    'If the task cannot start, put the reason in `blockers` and still fill in what you can.',
  ].join('\n'),
  { label: 'prepare', phase: 'Prepare', model: 'sonnet', schema: PREP },
  'prepare',
)

if (!prep) return { ok: false, verdict: 'BLOCKED', reason: 'the Prepare stage returned nothing', fix_waves_used: 0, reviews_run: 0, hard_failures }
if (prep.blockers && prep.blockers.length) return { ok: false, verdict: 'BLOCKED', reason: 'Prepare reported blockers', blockers: prep.blockers, baseline: prep, fix_waves_used: 0, reviews_run: 0 }
if (prep.is_default_branch) return {
  ok: false, verdict: 'BLOCKED', fix_waves_used: 0, reviews_run: 0, baseline: prep,
  reason: `refusing to run on the default branch \`${prep.branch}\`: swarm-gate commits its integration, and this repository takes squash merges onto trunk only. Cut a working branch or a worktree and re-run.`,
}
log(`baseline ${String(prep.baseline_sha).slice(0, 8)} on ${prep.branch}; ${prep.dirty_files.length} dirty file(s); ${prep.required_checks.filter(c => c.blocking).length} blocking check(s)`)

// The commit every private tree is cut from. It comes from the PROBE (a measurement)
// rather than from `prep.baseline_sha` (a claim) whenever the probe is usable, and it
// is refreshed after each weave so wave N+1 really does start from wave N's result.
let baseSha = prep.baseline_sha

phase('Measure')
const dirtyBase = await probeDirty('baseline')
if (dirtyBase && dirtyBase.HEAD_SHA) { baseSha = dirtyBase.HEAD_SHA; log(`workers will cut their trees from measured HEAD ${baseSha.slice(0, 8)}`) }
if (!dirtyBase) log('! no usable baseline digest — PASS is unreachable by construction from here (fail-closed), but the run continues so the work still gets done and reviewed')

// ============================== 2. Decompose ================================

phase('Decompose')
function decomposePrompt(retryNote) {
  return [
    preamble(prep), '',
    '## Your stage: DECOMPOSE. You write no product code.',
    retryNote || '',
    '',
    'Split the task into work packages a Sonnet worker can each finish IN ONE PASS.',
    'Sizing is the decision that makes or breaks this run: a package that needs three attempts costs more than three packages that need one. Split by RESPONSIBILITY, and if a package would rewrite more than roughly a thousand lines, split it again.',
    `**Hard ceiling: at most ${MAX_PACKAGES} packages.** If the task genuinely does not fit, say so in \`risks\` and produce the best ${MAX_PACKAGES}-package cut of the part that does — do not smuggle the overflow in as a giant final package.`,
    '',
    'Rules the decomposition must satisfy:',
    '- `files_owned` sets must be DISJOINT across ALL packages — not merely across concurrent ones. Sequencing two packages onto one file is NOT a solution: it hides serialisation inside what looks like a decomposition, and it is how a swarm quietly becomes a queue. If two packages want one file, either they are one package or that file needs splitting.',
    '- **A chain is not a decomposition.** If your `depends_on` edges form a path longer than 3, you have sequenced the work rather than divided it, and the wall-clock cost is the SUM of that path however high `maxWorkers` is. Prefer a wide graph with a short critical path.',
    '- Each wave IS merged back onto the session branch before the next is cut, so a later package really does find an earlier one\'s files on disk. Depend freely — but depend for a REASON, because every edge lengthens the critical path.',
    '- A file several packages must change is owned by NOBODY: list it in `shared_files`, and each package reports its intended edit as a `shared_file_delta` for the integrator to apply.',
    '- `depends_on` is real ordering only. Do not serialise what could run in parallel; do not parallelise what shares a contract.',
    '- Anything more than one package depends on — a type, a trait, a signature, a schema — goes in `shared_interfaces`. Those are FROZEN before parallel work starts, so nobody designs against a moving target.',
    '- Each `prompt` must stand alone: a worker sees only it, the preamble, and the repository.',
  ].filter(Boolean).join('\n')
}

let plan = await tryAgent(decomposePrompt(null), { label: 'decompose', phase: 'Decompose', model: 'sonnet', schema: DECOMP }, 'decompose')
if (plan && plan.packages.length > MAX_PACKAGES) {
  log(`! decomposer produced ${plan.packages.length} packages, ceiling is ${MAX_PACKAGES} — ONE consolidation attempt`)
  const again = await tryAgent(
    decomposePrompt(`\n**Your previous attempt produced ${plan.packages.length} packages, over the ceiling of ${MAX_PACKAGES}.** Consolidate — merge packages that share a concern or a dependency edge. Do not drop scope silently: if something must be left out, name it in \`risks\`.`),
    { label: 'decompose (consolidate)', phase: 'Decompose', model: 'sonnet', schema: DECOMP }, 'decompose/consolidate',
  )
  if (again) plan = again
}
if (!plan || !plan.packages || !plan.packages.length) {
  return { ok: false, verdict: 'BLOCKED', reason: 'the Decompose stage produced no packages', baseline: prep, fix_waves_used: 0, reviews_run: 0, hard_failures }
}
if (plan.packages.length > MAX_PACKAGES) {
  return {
    ok: false, verdict: 'REPLAN', fix_waves_used: 0, reviews_run: 0, baseline: prep, packages: plan.packages, hard_failures,
    reason: `decomposition still needs ${plan.packages.length} packages against a ceiling of ${MAX_PACKAGES}. Refusing to launch that many workers on a decomposition that may itself be wrong — raise maxPackages deliberately, or split the task upstream.`,
  }
}

const SHARED_FILES = plan.shared_files || []
const sharedNote = SHARED_FILES.length
  ? `\n## Files NO worker may edit (the integrator owns them)\n${SHARED_FILES.map(f => `- ${f}`).join('\n')}\nReport the edit you need as a \`shared_file_deltas\` entry instead of making it.`
  : ''

// -------- 2b. Contract freeze (only when something is genuinely shared) -----

const ifaces = plan.shared_interfaces || []
let contract = null
if (ifaces.length) {
  phase('Contract')
  log(`freezing ${ifaces.length} shared interface(s) before any parallel work`)
  contract = await tryAgent(
    [
      preamble(prep), sharedNote, '',
      '## Your stage: CONTRACT FREEZE. You run ALONE — no other worker is active.', '',
      'Land ONLY the shared interfaces below: the types, signatures and trait bounds more than one package depends on. Bodies may be stubs where a package will fill them; the SHAPE is what must be right, because every parallel worker is about to compile against it.', '',
      ifaces.map(i => `### ${i.name}\nfiles: ${(i.files || []).join(', ')}\n${i.spec}`).join('\n\n'), '',
      'Make it compile. Commit on the current branch, naming this as the contract freeze. Then report exactly what you landed, so the workers are told the truth about what exists.',
    ].join('\n'),
    { label: 'contract', phase: 'Contract', model: 'sonnet' }, 'contract freeze',
  )
  // The freeze COMMITS on the session branch, so the base every wave-1 worker cuts from has
  // moved. Run 4 (wf_508f8da1-d84) cut A and B2 from the pre-freeze sha and would have merged
  // them back over the contract they were supposed to compile against. Measure again — with the
  // same probe, so the dirty digests are compared too: a freeze that touched user work is a
  // hard failure, not a footnote.
  if (contract) {
    phase('Measure')
    const afterFreeze = await probeDirty('after-contract')
    const cmp = compareDirty(dirtyBase, afterFreeze, 'after the contract freeze')
    dirty_checks.push(cmp)
    if (cmp.ok === false) hard_failures.push(`contract freeze moved the user's uncommitted work: ${cmp.moved.join(', ')}`)
    if (afterFreeze && afterFreeze.HEAD_SHA) { baseSha = afterFreeze.HEAD_SHA; log(`contract froze at measured HEAD ${baseSha.slice(0, 8)}; wave 1 cuts from there`) }
    else log('! post-freeze probe unusable; wave 1 would cut from the PRE-freeze base — refusing')
    if (!afterFreeze || !afterFreeze.HEAD_SHA) hard_failures.push('post-freeze probe unusable: the wave-1 base could not be measured')
  }
}

// ============================== 3. Implement ================================

const WEAVE = {
  type: 'object', additionalProperties: false,
  required: ['merged', 'conflicts', 'head_sha', 'compiles'],
  properties: {
    merged: { type: 'array', items: { type: 'string' } },
    failed_to_merge: { type: 'array', items: { type: 'string' } },
    conflicts: { type: 'array', items: { type: 'string' } },
    head_sha: { type: 'string' },
    compiles: { type: 'boolean', description: 'honest: false is an ordinary mid-phase answer; a false claim of true costs the next wave' },
    notes: { type: 'array', items: { type: 'string' } },
  },
}

// Merge one wave onto the session branch so the NEXT wave's worktrees contain it.
function weave(sources, w, total) {
  return tryAgent(
    [
      preamble(prep), '',
      `## Your stage: WEAVE (after implementation wave ${w + 1} of ${total}). You run ALONE and own the whole repository.`, '',
      `Merge these worktree branches onto the current branch \`${prep.branch}\`, ONE AT A TIME, in the order given:\n${sources.map((r, i) => `${i + 1}. \`${r.branch}\`  (${r.worktree})\n   ${r.summary}`).join('\n')}`,
      SHARED_FILES.length ? `\n## Shared files only you may edit\n${SHARED_FILES.map(f => `- ${f}`).join('\n')}\nApply the deltas these workers reported:\n${sources.flatMap(r => r.shared_file_deltas || []).map(d => `- ${d}`).join('\n') || '- (none reported)'}` : '',
      '',
      '## Why this stage exists — it changes what you should optimise for',
      'The NEXT wave of workers gets a fresh worktree cut from whatever you commit here, and every package in it declared a dependency on something in this wave. If you do not commit, they will be handed a paragraph about code they cannot open.',
      'So the one thing you must not do is skip a branch because its work looks incomplete. Merge it, apply whatever `shared_files` delta it reported, and commit.',
      '',
      '## What you are NOT doing',
      'You are NOT running the check suite. Mid-phase the tree is legitimately half-migrated and need not compile; a green build is the final integration\'s job, and chasing one here would rewrite work the next wave is about to replace.',
      'Do try a cheap `cargo +nightly check --lib` and report `compiles` HONESTLY — `false` is expected and costs nothing. Claiming `true` when it is false costs the next wave its ability to check itself.',
      'Resolve merge conflicts. Do not push and do not merge into the default branch.',
      'COMMIT before you finish. An unwoven wave is a wave the next one cannot see, which is the exact failure this stage was added to fix.',
    ].filter(Boolean).join('\n'),
    { label: `weave:w${w + 1}`, phase: 'Weave', model: 'sonnet', schema: WEAVE },
    `weave after wave ${w + 1}`,
  )
}

phase('Implement')
const { waves, cycle } = toWaves(plan.packages)
log(`${plan.packages.length} package(s) in ${waves.length} dependency wave(s)${cycle ? ' (cycle detected — see log)' : ''}`)
if (waves.length > 3 && waves.length >= Math.ceil(plan.packages.length / 2)) {
  log(`! critical path is ${waves.length} waves deep over ${plan.packages.length} packages — closer to a queue than a swarm; wall-clock will be the SUM of that path whatever maxWorkers=${MAX_WORKERS} says`)
  hard_failures.push(`decomposition has a ${waves.length}-wave critical path over ${plan.packages.length} packages: the work was sequenced rather than divided`)
}

const implementations = []
const weaves = []
for (let w = 0; w < waves.length; w++) {
  const wave = waves[w]
  log(`wave ${w + 1}/${waves.length}: ${wave.map(p => p.id).join(', ')}`)
  const done = implementations.filter(r => r && r.status !== 'blocked')
  const doneNote = done.length
    ? `\n## Already landed by earlier waves — and it is ON DISK in your worktree, not merely described here\n${done.map(r => `- ${r.id}: ${r.summary}`).join('\n')}\nRead that code before extending it; these are files that exist.`
    : ''

  const got = await inWaves(wave, MAX_WORKERS, (pkg) => tryAgent(
    [
      preamble(prep), sharedNote, doneNote,
      contract ? `\n## The frozen contract you build against\n${contract}` : '', '',
      `## Your package: ${pkg.id} — ${pkg.title}`, '',
      `**You own these files and only these:**\n${pkg.files_owned.map(f => `- ${f}`).join('\n')}`, '',
      pkg.prompt, '', `**Acceptance:** ${pkg.acceptance}`, '',
      ownWorktree(baseSha, pkg.id.replace(/[^a-z0-9]+/gi, '-')), '',
      '## How you work',
      'That worktree contains every earlier wave, because the base above is the commit the last weave produced — measured, not claimed.',
      'If a file an earlier package was supposed to create is still NOT there, STOP and report `blocked` naming that file. Do not recreate it: two workers writing one file from two sides is the failure this workflow exists to prevent, and a missing dependency is a real defect worth surfacing rather than working around.',
      LANE_CHECKS
        ? 'Verify your own package builds before you finish.'
        : 'Do NOT run a whole-project build or test suite. A dedicated integration stage owns that, and N concurrent builds fight over one lock and one disk while telling each of you about the others\' half-finished edits. Check yourself by READING the code.',
      'COMMIT your work in your worktree — the next wave is cut from a branch built out of these commits, so uncommitted work is work that never happened.',
      'Report your branch (`git branch --show-current`), your worktree path (`git rev-parse --show-toplevel`) and the files you changed.',
      'If you cannot finish, say `partial` or `blocked` and name precisely what is missing. Do not report `done` for a stub.',
    ].join('\n'),
    { label: `impl:${pkg.id}`, phase: 'Implement', model: 'sonnet', schema: WORK },
    `package ${pkg.id}`,
  ))

  const waveLanded = []
  for (const { item, result } of got) {
    if (!result) { implementations.push({ id: item.id, status: 'blocked', branch: '', worktree: '', files_changed: [], summary: 'worker returned no result' }); continue }
    if (result.status !== 'done') hard_failures.push(`package ${result.id}: ${result.status} — ${(result.open_problems || []).join('; ') || result.summary}`)
    implementations.push(result)
    if (result.branch) waveLanded.push(result)
  }

  if (waveLanded.length) {
    phase('Weave')
    const woven = await weave(waveLanded, w, waves.length)
    weaves.push(woven)
    if (woven) {
      log(`  wove ${woven.merged.length}/${waveLanded.length} branch(es) onto ${prep.branch} at ${String(woven.head_sha).slice(0, 8)}; compiles=${woven.compiles}`)
      // Re-measure rather than trust the weave's own report: the next wave's trees are
      // cut from this sha, and a wrong one is the failure that cost a whole run.
      const after = await probeDirty(`after-wave-${w + 1}`)
      if (after && after.HEAD_SHA) { baseSha = after.HEAD_SHA; log(`  next wave cuts from measured ${baseSha.slice(0, 8)}`) }
      else if (woven.head_sha) { baseSha = woven.head_sha; log(`  ! probe unusable; falling back to the weave's claimed ${String(baseSha).slice(0, 8)}`) }
      if ((woven.failed_to_merge || []).length) hard_failures.push(`wave ${w + 1}: branches that would not merge: ${woven.failed_to_merge.join(', ')}`)
    } else {
      hard_failures.push(`wave ${w + 1} was never woven onto the session branch — every later wave was cut without it`)
    }
    if (w + 1 < waves.length) phase('Implement')
  } else {
    log(`! wave ${w + 1} produced nothing to weave`)
  }
}

const landed = implementations.filter(r => r.branch && r.status !== 'blocked')
if (landed.length && !weaves.some(w => w && (w.merged || []).length)) {
  hard_failures.push('no implementation wave was successfully woven onto the session branch — the integration below is running over the contract freeze alone')
}
if (!landed.length) {
  return { ok: false, verdict: 'BLOCKED', reason: 'no package produced a mergeable branch', baseline: prep, packages: plan.packages, implementations, hard_failures, fix_waves_used: 0, reviews_run: 0 }
}

// ============ 4-7. integrate -> review -> fix, bounded by maxFixRounds ======

function integrate(sources, wave, extraNotes, label) {
  return tryAgent(
    [
      preamble(prep), '',
      `## Your stage: INTEGRATE (${label}). You own the WHOLE repository. Every worker has finished.`, '',
      sources.length
        ? `Merge these worktree branches into the current branch \`${prep.branch}\`, ONE AT A TIME, in the order given:\n${sources.map((s, i) => `${i + 1}. \`${s.branch}\`  (${s.worktree})\n   ${s.summary || ''}`).join('\n')}`
        : 'Nothing new to merge. Re-run the checks against what is already committed.',
      SHARED_FILES.length ? `\n## Shared files only you may edit\n${SHARED_FILES.map(f => `- ${f}`).join('\n')}\nApply the deltas the workers reported:\n${sources.flatMap(s => s.shared_file_deltas || []).map(d => `- ${d}`).join('\n') || '- (none reported)'}` : '',
      extraNotes || '', '',
      '## Then',
      '1. Make the MERGED tree compile and behave. Fix integration breakage yourself — that is the job. Do not delete a test to make it pass: a failing test is either a real defect or a stale expectation, and you must say which.',
      `2. Run every required check and report each result honestly:\n${prep.required_checks.map(c => `   - [${c.blocking ? 'BLOCKING' : 'advisory'}] ${c.name}: \`${c.command}\``).join('\n')}`,
      prep.dirty_files.length
        ? `3. Leave these baseline-dirty files EXACTLY as you found them — they are the user's own uncommitted work:\n${prep.dirty_files.map(f => `   - ${f}`).join('\n')}\n   You are not asked to confirm this and there is no field in which to claim it: an independent probe digests the tree before and after you, this workflow compares the digests itself, and a change it cannot explain blocks the run.`
        : '3. There was no pre-existing uncommitted work to preserve.',
      '4. COMMIT everything you changed, on the current branch. Two reasons, both hard: the reviewer reads a fresh worktree cut from your commit, so an uncommitted merge is invisible to it; and the independent probe cannot tell your leftover uncommitted work from a modified user file, so it will read one as the other and block the run.',
      '5. Report `git diff --stat` against the baseline and the new HEAD sha.', '',
      'Do NOT push. Do NOT merge into the default branch. Do NOT open a PR.',
      '`all_blocking_passed` is a field the workflow gates on — reporting it true while a blocking check failed is the single most damaging thing you can do here.',
    ].filter(Boolean).join('\n'),
    { label: `integrate:${label}`, phase: 'Integrate', model: 'sonnet', schema: INTEG },
    `integration (${label})`,
  )
}

function review(wave, integ, priorOpen, workerClaims) {
  return tryAgent(
    [
      preamble(prep), '',
      `## Your stage: REVIEW (wave ${wave} of at most ${MAX_FIX_ROUNDS}). You are the gate. You are read-only.`, '',
      ownWorktree(baseSha, `review-w${wave}`),
      'That worktree is THROWAWAY: nothing you write there reaches anybody, which is how "the reviewer does not fix code" is made structural rather than merely asked for. Do not try to fix what you find — describe it precisely enough that a fixer needs no further investigation.', '',
      '## YOUR SOURCES OF TRUTH, in this order — and nothing else counts as evidence',
      '1. The original task/specification quoted above.',
      '2. **The repository at the reviewed HEAD.** Read the actual code.',
      `3. The full diff: \`git diff ${prep.baseline_sha}..HEAD\`. Read it yourself; do not substitute the summary below for it.`,
      '4. The blocking-check output the integrator reported.',
      '',
      `### Diff summary (an index, not evidence)\n\`\`\`\n${integ.diff_stat}\n\`\`\`\nIntegrated at ${integ.head_sha}, from baseline ${prep.baseline_sha}.`,
      `### Check results\n${(integ.checks || []).map(c => `- ${c.passed ? 'PASS' : 'FAIL'} [${c.blocking ? 'blocking' : 'advisory'}] ${c.name}: \`${c.command}\``).join('\n')}`,
      (integ.conflicts || []).length ? `### Merge conflicts resolved by hand — prime suspects\n${integ.conflicts.map(c => `- ${c}`).join('\n')}` : '',
      '',
      '## SECONDARY MATERIAL — claims, not evidence',
      'Below is what the workers said they did. **Do not build your review on it.** It is here for exactly one purpose: to let you catch a gap between what was CLAIMED and what the code actually does. A worker reporting `done` over a stub is a finding, and it is invisible to anyone who reads the claim as the truth. Where a claim and the code disagree, the code wins and the disagreement is itself worth reporting.',
      workerClaims.map(c => `- [${c.status}] ${c.id}: ${c.summary}`).join('\n') || '- (none)',
      priorOpen.length
        ? `\n## Findings from earlier waves — verify each INDEPENDENTLY, in the code\n${priorOpen.map(f => `- \`${f.key}\` [${f.priority}] ${f.title}\n  verify by: ${f.how_to_verify}`).join('\n')}\nPut the keys you confirmed fixed in \`closed_previous\`, the ones still broken in \`still_open_previous\` (and re-report them in \`findings\` with THE SAME key). A fix you did not check is not a fix you may close.`
        : '',
      '',
      '## Review the WHOLE task, not just the newest diff',
      'Judge the result against the acceptance criteria. Ask, in this order:',
      '1. **Does it do what was asked** — including the parts nobody would notice missing?',
      '2. **Is it correct** — the concrete input or sequence producing a wrong result, a leak, a panic, a lost write?',
      '3. **Is it a real change or an adapter** — was the old path retired, or does dead code still shadow the new one so an edit to the wrong file compiles and changes nothing?',
      '4. **Do the tests discriminate** — for each new test, name the edit to the product code that would make it fail. If there is none it is not a test, and that is a finding. Where it is cheap, MUTATE the code and watch the test to be sure.',
      '5. **Did anything get lost** — a deleted regression pin, a dropped behaviour, a doc claim the change made false.',
      '',
      'Every finding needs a STABLE `key` (kebab-case, describing the defect rather than its position) and a `how_to_verify` a fixer can run. Re-use an earlier wave\'s key when you re-report its finding.',
      '',
      '## The verdict, and when to use REPLAN',
      '- `PASS` — you would ship this.',
      '- `FIX` — there are defects, but the decomposition and the approach were right; a fixer can close each finding where it stands.',
      `- **\`REPLAN\`** — the problem is NOT a list of local defects. The architecture, the decomposition, or the chosen approach is wrong, and handing ${MAX_FINDINGS} findings to parallel fixers would be paying to patch something that has to be rebuilt. Say why in \`replan_reason\`. Choosing this STOPS the fix loop, which is the point: it is cheaper to replan than to fix a design.`,
      '- `BLOCKED` — you could not review (checks did not run, the diff is unreadable, the result is unrecognisable as the task).',
      '',
      'A failed BLOCKING check means the verdict cannot be PASS, whatever the code looks like.',
    ].filter(Boolean).join('\n'),
    { label: `review:wave${wave}`, phase: 'Review', model: 'opus', schema: REVIEW },
    `review wave ${wave}`,
  )
}

let integ = await integrate([], 0, `\n## Every implementation wave has ALREADY been woven onto this branch and committed — ${weaves.filter(Boolean).length} weave(s), the last reporting compiles=${weaves.filter(Boolean).slice(-1).map(w => w.compiles)[0]}. Nothing is left to merge: your job is to make the whole thing GREEN and run every check.`, 'wave 0')
if (!integ) return { ok: false, verdict: 'BLOCKED', reason: 'the first Integrate stage returned nothing', baseline: prep, packages: plan.packages, implementations, hard_failures, fix_waves_used: 0, reviews_run: 0 }
integrations.push(integ)

phase('Measure')
const p0 = await probeDirty('after-wave-0')
if (p0 && p0.HEAD_SHA) baseSha = p0.HEAD_SHA
dirty_checks.push(compareDirty(dirtyBase, p0, 'after-wave-0'))
log(`integration wave 0: ${integ.merged.length} branch(es) merged, blocking checks ${integ.all_blocking_passed ? 'PASS' : 'FAIL'}, user work ${dirty_checks[0].ok === true ? 'intact' : dirty_checks[0].ok === false ? 'CHANGED' : 'unverified'}`)

const openFindings = new Map()
let fixWaves = 0
let verdict = 'BLOCKED'
let replanReason = null
const workerClaims = implementations.map(r => ({ id: r.id, status: r.status, summary: r.summary }))

for (;;) {
  phase('Review')
  const rv = await review(fixWaves, integ, [...openFindings.values()], workerClaims)
  if (!rv) { verdict = 'BLOCKED'; break }

  // No silent caps: a reviewer over the ceiling gets truncated by PRIORITY and
  // the drop is named, because "30 findings" reading as "all of them" is the
  // exact failure this rule exists to prevent.
  let findings = rv.findings.slice().sort((a, b) => (PRIORITY_ORDER[a.priority] ?? 9) - (PRIORITY_ORDER[b.priority] ?? 9))
  if (findings.length > MAX_FINDINGS) {
    const dropped = findings.slice(MAX_FINDINGS)
    log(`! review returned ${findings.length} findings, ceiling ${MAX_FINDINGS} — DROPPING ${dropped.length} lowest-priority: ${dropped.map(f => f.key).join(', ')}`)
    hard_failures.push(`review wave ${fixWaves}: ${dropped.length} finding(s) dropped at the maxReviewFindings ceiling — the result is not a clean bill of health`)
    findings = findings.slice(0, MAX_FINDINGS)
  }

  reviews.push({ wave: fixWaves, verdict: rv.verdict, summary: rv.summary, replan_reason: rv.replan_reason, findings, closed_previous: rv.closed_previous || [], verified_good: rv.verified_good || [] })
  for (const k of (rv.closed_previous || [])) openFindings.delete(k)
  for (const f of findings) openFindings.set(f.key, f)
  verdict = rv.verdict
  log(`review wave ${fixWaves}: ${rv.verdict}, ${findings.length} finding(s), ${(rv.closed_previous || []).length} closed, ${openFindings.size} open`)

  if (verdict === 'REPLAN') { replanReason = rv.replan_reason || rv.summary; log('! REPLAN — the decomposition or the approach is wrong; stopping the fix loop rather than paying to patch a design'); break }
  if (verdict === 'PASS' && openFindings.size === 0) break
  if (verdict === 'BLOCKED') { log('! review returned BLOCKED — a fix wave cannot help; stopping'); break }
  if (fixWaves >= MAX_FIX_ROUNDS) { log(`! fix-wave ceiling reached (${MAX_FIX_ROUNDS}); stopping with ${openFindings.size} finding(s) open rather than looping`); break }
  if (budget.total && budget.remaining() < 60000) { log(`! stopping early: ${Math.round(budget.remaining() / 1000)}k tokens left, below the reserve for a fix wave`); break }

  // -------------------------- 6. Fix ---------------------------------------
  fixWaves += 1
  phase('Fix')
  const fixBase = integ.head_sha
  const groups = clusterByFiles([...openFindings.values()], f => f.files)
  log(`fix wave ${fixWaves}: ${openFindings.size} finding(s) in ${groups.length} group(s) disjoint by DECLARED files`)

  const fixes = await inWaves(groups, MAX_WORKERS, (group, gi) => tryAgent(
    [
      preamble(prep), sharedNote, '',
      `## Your stage: FIX (wave ${fixWaves}, group ${gi + 1}/${groups.length}).`, '',
      `**Findings in this group name these files:**\n${filesOf(group).map(f => `- ${f}`).join('\n') || '- (none named — establish the scope yourself and stay minimal)'}`,
      'Other fixers are working right now on findings that named OTHER files. If your fix turns out to need a file outside the list above, MAKE IT ANYWAY and report it in `files_changed` — an independent probe reads what your branch really touched, and an unexpected overlap is handled by a serial lane rather than by a blind merge. Hiding the extra edit is the only thing that would break that.',
      '', '## The findings you own',
      group.map(f => `### \`${f.key}\` [${f.priority}] ${f.title}\nfiles: ${(f.files || []).join(', ') || '(unspecified)'}${f.line ? `:${f.line}` : ''}\n\n${f.explanation}\n\n**Verify by:** ${f.how_to_verify}`).join('\n\n'),
      '', ownWorktree(baseSha, `fix-w${fixWaves}-g${gi + 1}`), '',
      '## How you work',
      'Fix the findings you were given and NOTHING else — no opportunistic refactors, no unrelated cleanup; a fix wave that also rewrites something else is a fix wave nobody can review.',
      'Where a finding names a regression, add or repair a test that would FAIL against the defect and passes after the fix. A test that cannot fail is worse than no test — before you keep one, name the edit that would make it red.',
      LANE_CHECKS ? 'Verify your files build before you finish.' : 'Do NOT run a whole-project build. The integrator owns that.',
      'COMMIT in your worktree and report your branch, your worktree path, and which finding keys you actually addressed.',
      'If you decline a finding, put its key in `declined` WITH the reason — an unexplained silent skip reads as a fix to the next reviewer.',
    ].join('\n'),
    { label: `fix:w${fixWaves}:g${gi + 1}`, phase: 'Fix', model: 'sonnet', schema: FIXED },
    `fix wave ${fixWaves} group ${gi + 1}`,
  ))

  const fixResults = []
  for (let i = 0; i < fixes.length; i++) {
    const r = fixes[i].result
    if (!r) continue
    if (r.status !== 'done') hard_failures.push(`fix wave ${fixWaves} group ${r.group || i + 1}: ${r.status} — ${(r.open_problems || []).join('; ')}`)
    if (r.declined && r.declined.length) log(`  group ${i + 1} declined: ${r.declined.join(', ')}`)
    if (r.branch) fixResults.push(r)
  }
  if (!fixResults.length) { hard_failures.push(`fix wave ${fixWaves} produced no mergeable branch`); log('! no fix branch to merge — stopping'); break }

  // --- what the branches REALLY touched, measured rather than declared ------
  // A finding filed against foo.rs can need bar.rs to fix. Grouping by the
  // reviewer's declared files is a guess; this is the measurement, and it is
  // taken by a probe with no stake in the answer.
  phase('Measure')
  const touched = await tryAgent(
    [
      'You are a measurement probe. One job, no opinion.',
      `For EACH branch below, run EXACTLY:\n\`git -C ${REPO} diff --name-only ${fixBase}..<branch>\`\nand return its stdout lines verbatim as \`files\`.`,
      '', fixResults.map(r => `- ${r.branch}`).join('\n'), '',
      'Change nothing, commit nothing, check out nothing. If a branch is unknown, put the error in that entry\'s `error` and return an empty `files` list. Never guess a filename.',
    ].join('\n'),
    { label: `probe:touched:w${fixWaves}`, phase: 'Measure', model: 'haiku', schema: TOUCHED },
    `touched-files probe (wave ${fixWaves})`,
  )

  const realFiles = new Map()
  if (touched) for (const b of touched.branches) realFiles.set(b.branch, (b.files || []).filter(Boolean))
  // A branch the probe could not measure is treated as overlapping everything:
  // unmeasured is not the same as disjoint.
  const measured = fixResults.map(r => ({ r, files: realFiles.has(r.branch) ? realFiles.get(r.branch) : null }))
  const unmeasured = measured.filter(m => m.files === null)
  if (unmeasured.length) log(`! ${unmeasured.length} fix branch(es) could not be measured — treating them as overlapping (serial lane)`)

  const clusters = clusterByFiles(measured, m => m.files)
  const leaders = [], followers = []
  for (const c of clusters) { leaders.push(c[0]); for (const extra of c.slice(1)) followers.push(extra) }
  if (followers.length) {
    log(`! ${followers.length} fix branch(es) really touch a file another branch also touched — NOT merging them in parallel; they go to the serial reapply lane`)
    for (const f of followers) log(`    ${f.r.branch}: ${(f.files || ['(unmeasured)']).join(', ')}`)
  } else {
    log(`  all ${leaders.length} fix branch(es) verified file-disjoint by measurement`)
  }

  phase('Integrate')
  const merged = await integrate(
    leaders.map(m => ({ branch: m.r.branch, worktree: m.r.worktree, summary: `fixes ${(m.r.addressed || []).join(', ')}`, shared_file_deltas: [] })),
    fixWaves,
    `\n## These branches are FIXES for reviewer findings\nKeys addressed: ${leaders.flatMap(m => m.r.addressed || []).join(', ') || '(none reported)'}\nIf two fixes contradict each other, that contradiction is itself a defect — resolve it and say how in \`notes\`.`,
    `fix wave ${fixWaves}`,
  )
  if (!merged) { verdict = 'BLOCKED'; break }
  integrations.push(merged); integ = merged

  // --- serial reapply lane: a real rebase/fix, never a parallel merge -------
  if (followers.length) {
    phase('Reapply')
    for (let fi = 0; fi < followers.length; fi++) {
      const m = followers[fi]
      const keys = m.r.addressed || []
      const items = keys.map(k => openFindings.get(k)).filter(Boolean)
      const done = await tryAgent(
        [
          preamble(prep), '',
          `## Your stage: REAPPLY (serial lane, ${fi + 1}/${followers.length}). You are the ONLY agent running.`, '',
          `Branch \`${m.r.branch}\` fixed the findings below, but it turned out to touch ${m.files ? `files another branch also touched (${m.files.join(', ')})` : 'files that could not be measured'}. Merging it in parallel with that branch could silently drop or duplicate an edit, so it was deliberately NOT merged.`,
          `Its work is still there: \`git diff ${fixBase}..${m.r.branch}\`. **Read it, then RE-APPLY its intent on the current integrated tree** — as a fresh fix, not as a textual merge. Where its change and what is now on the branch disagree, the finding below is the arbiter, not the older diff.`,
          '', '## The findings it was meant to close',
          items.length ? items.map(f => `### \`${f.key}\` [${f.priority}] ${f.title}\n${f.explanation}\n\n**Verify by:** ${f.how_to_verify}`).join('\n\n') : `(the branch reported closing: ${keys.join(', ') || 'nothing'})`,
          '', 'Work directly in the current repository — you are serial, nobody else is writing. Commit when done. Do not run the full check suite; a check pass follows you.',
        ].join('\n'),
        { label: `reapply:w${fixWaves}:${fi + 1}`, phase: 'Reapply', model: 'sonnet' },
        `reapply ${m.r.branch}`,
      )
      if (!done) hard_failures.push(`reapply of ${m.r.branch} failed — findings ${keys.join(', ')} are NOT closed`)
    }
    phase('Integrate')
    const after = await integrate([], fixWaves, '\n## Nothing to merge — the serial reapply lane just rewrote part of this tree. Re-run every check against the final state.', `fix wave ${fixWaves} recheck`)
    if (after) { integrations.push(after); integ = after } else { verdict = 'BLOCKED'; break }
  }

  phase('Measure')
  const fixProbe = await probeDirty(`after-fix-wave-${fixWaves}`)
  if (fixProbe && fixProbe.HEAD_SHA) baseSha = fixProbe.HEAD_SHA
  const dc = compareDirty(dirtyBase, fixProbe, `after-fix-wave-${fixWaves}`)
  dirty_checks.push(dc)
  log(`integration wave ${fixWaves}: blocking checks ${integ.all_blocking_passed ? 'PASS' : 'FAIL'}, user work ${dc.ok === true ? 'intact' : dc.ok === false ? 'CHANGED' : 'unverified'}`)
}

// ============================== 8. Report ===================================

const lastDirty = dirty_checks.length ? dirty_checks[dirty_checks.length - 1] : { ok: null, reason: 'never measured' }

// The probe measures HEAD too, so the sha the integrator REPORTED can be checked
// against one it never saw. A mismatch means the diff that was reviewed is not
// the tree that exists — which is worth more than either number alone.
const headClaimed = String(integ.head_sha || '').toLowerCase()
const headMeasured = String(lastDirty.head_measured || '').toLowerCase()
const headAgrees = !headClaimed || !headMeasured
  ? null
  : (headClaimed.startsWith(headMeasured) || headMeasured.startsWith(headClaimed))
if (headAgrees === false) hard_failures.push(`HEAD disagreement: the integrator reported ${headClaimed}, an independent probe measured ${headMeasured} — the reviewed diff may not describe the tree you have`)
const failedBlocking = (integ.checks || []).filter(c => c.blocking !== false && !c.passed)

// PASS is a conjunction computed HERE. Note `lastDirty.ok === true`: a null
// (unmeasurable) is not a pass, and the term the integrator reports about its
// own tidiness is not one of the terms.
const ok =
  verdict === 'PASS' &&
  openFindings.size === 0 &&
  integ.all_blocking_passed === true &&
  failedBlocking.length === 0 &&
  lastDirty.ok === true &&
  hard_failures.length === 0

const why = [
  verdict !== 'PASS' ? `reviewer verdict ${verdict}` : null,
  openFindings.size ? `${openFindings.size} finding(s) still open` : null,
  integ.all_blocking_passed === true ? null : 'blocking checks did not all pass',
  failedBlocking.length ? `failed: ${failedBlocking.map(c => c.name).join(', ')}` : null,
  lastDirty.ok === true ? null : (lastDirty.ok === false
    ? `the user's uncommitted work changed (${(lastDirty.moved || []).join(', ')}) — either a baseline-dirty file was modified, or the integrator left work uncommitted`
    : `could not verify the user's uncommitted work was preserved (${lastDirty.reason || 'unknown'})`),
  headAgrees === false ? 'the integrator\'s reported HEAD does not match the measured one' : null,
  hard_failures.length ? `${hard_failures.length} hard failure(s)` : null,
].filter(Boolean)

log(ok ? `RESULT: PASSED after ${fixWaves} fix wave(s) and ${reviews.length} review(s)` : `RESULT: NOT PASSED — ${why.join('; ')}`)

// The diff a reviewer reads can include commits that are orchestration
// infrastructure rather than the task itself — e.g. a hand-applied fix to
// this very workflow script, landed directly on the session branch mid-run
// (a Contract-phase re-measure commit is exactly this shape: real, correct,
// and not the task). This branch lands on trunk as ONE squash commit whose
// message is "the change's own account" (AGENTS.md), so an infra file riding
// inside that squash reads to `git log`, the release audit and `git bisect`
// as part of the task unless someone notices and splits it out. Surface it
// here instead of leaving it to a reviewer to catch by hand every run.
const harnessFiles = String(integ.diff_stat || '').split('\n')
  .map(l => l.trim())
  .filter(l => l.includes('.claude/workflows/'))
  .map(l => l.split('|')[0].trim())
  .filter(Boolean)
if (harnessFiles.length) log(`! diff touches orchestration infra outside the task (${harnessFiles.join(', ')}) — decide at squash time whether to split it out or name it in the message`)

return {
  ok,
  harness_files_touched: harnessFiles,
  verdict: ok ? 'PASS' : (verdict === 'REPLAN' ? 'REPLAN' : verdict === 'PASS' ? 'FIX' : verdict),
  replan_reason: replanReason,
  not_passed_because: ok ? [] : why,
  fix_waves_used: fixWaves,
  reviews_run: reviews.length,
  limits: { max_workers: MAX_WORKERS, max_fix_rounds: MAX_FIX_ROUNDS, max_packages: MAX_PACKAGES, max_review_findings: MAX_FINDINGS, max_agent_retries: MAX_RETRIES },
  baseline: { branch: prep.branch, sha: prep.baseline_sha, dirty_files: prep.dirty_files },
  acceptance_criteria: prep.acceptance_criteria,
  packages: plan.packages.map(p => ({ id: p.id, title: p.title, files_owned: p.files_owned, depends_on: p.depends_on })),
  implementations,
  integrations,
  user_work_preserved: {
    verdict: lastDirty.ok,
    checks: dirty_checks,
    method: 'sha256 of `git diff --binary`, of `git diff --cached --binary`, and of the sorted untracked path+content set. Taken by an independent Haiku probe that is given no task, no criteria and no stake, in a fixed four-line KEY=VALUE protocol; parsed and compared by this workflow. No agent reports on this and none has a field in which to claim it.',
    not_covered: 'gitignored files are excluded (`--exclude-standard`): digesting them would mean hashing this repository\'s multi-gigabyte build trees every wave. They are protected by worker instructions and the outbound guard, not by this measurement.',
  },
  final: { head_sha: integ.head_sha, head_sha_measured: lastDirty.head_measured || null, head_agrees: headAgrees, diff_stat: integ.diff_stat, checks: integ.checks },
  reviews,
  open_findings: [...openFindings.values()],
  hard_failures,
  next_step: [
    verdict === 'REPLAN'
      ? 'The reviewer says the approach itself is wrong. Read `replan_reason` and re-cut the task before spending another fix wave.'
      : ok
        ? 'Nothing was pushed and nothing was merged into the default branch — the work sits committed on the working branch for you to land.'
        : 'Read `not_passed_because`, `open_findings` and `hard_failures`. Re-run with a higher maxFixRounds, or finish the remainder by hand.',
    harnessFiles.length
      ? `Also: this branch's diff touches orchestration infra outside the task (${harnessFiles.join(', ')}) — see \`harness_files_touched\`. Squashing lands it inside a commit message that describes the task; split it out onto its own commit first, or name it explicitly in the squash message, rather than letting \`git log\`/bisect/the release audit read it as the task.`
      : null,
  ].filter(Boolean).join(' '),
}

// The preamble every worker and reviewer opens with. Declared last on purpose:
// function declarations hoist, and keeping it out of the reading path above
// means the stage sequence is what you see first.
function preamble(p) {
  return [
    'You are one agent in a `swarm-gate` run. Other agents may be working right now.',
    '', '## The task', TASK, '',
    `## Acceptance criteria\n${p.acceptance_criteria.map(c => `- ${c}`).join('\n')}`,
    p.project_rules && p.project_rules.length ? `\n## Project rules that bind you\n${p.project_rules.map(r => `- ${r}`).join('\n')}` : '',
    p.dirty_files && p.dirty_files.length
      ? `\n## DO NOT TOUCH — uncommitted work already in the tree at baseline ${p.baseline_sha}\n${p.dirty_files.map(f => `- ${f}`).join('\n')}\nThese are the user's own changes. Losing one is worse than failing the task, and an independent probe digests them before and after every stage.`
      : '',
    '', '## Hard rules',
    '- NEVER `git push`, never open or comment on a PR, never merge into the default branch, never publish anything.',
    '- Never bare `git stash` / `git stash pop` — the stash stack is shared between worktrees.',
    '- Do not spawn subagents and do not start another workflow. You are a leaf.',
    '- Stay inside the files you were given. Another agent owns the rest.',
    '- Report what is true. A partial result reported as `done` is the one outcome this whole structure exists to prevent.',
  ].filter(Boolean).join('\n')
}
