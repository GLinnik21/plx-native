# The scanner behind ci/check-deps.sh's ledger rules (blocking, workers, globals, forks, fxlanes).
# Any number of Rust files per run (repo-relative paths):   awk -f ci/ledger.awk <path> <path> ...
# It prints one line per PRODUCTION site, keyed so a ledger can hold the site as an exact multiset:
#
#     <path>::<enclosing fn>                 blocking, forks   (forks: a field under a cfg is keyed by its struct)
#     <path>::<enclosing fn>::<spelling>     workers, globals   (LG_KEYSPELL=1)
#     <path>::AppFx::<Variant>               fxlanes            (LG_MODE=arm; `AppFx::_` for an arm with no variant)
#
# and, in call mode, `REF<TAB><path>:<line>` for a line that names the identifier without calling it
# (an alias the call-site grep cannot count). Everything is configured through the environment,
# because awk's -v would process the backslashes of a regex:
#
#   LG_MODE      call (default) | fork | arm
#   -- call: every occurrence of LG_SITE on a code line is a site --
#   LG_SITE      ERE of one call site, `(^|[^A-Za-z0-9_])name\(` style (awk has no \b)
#   LG_DEF       ERE of a definition header (`fn name(`): blanked before matching, so it is neither a
#                site nor a mention (the rest of its line still is)
#   LG_REF       ERE (or empty); a line matching it names the identifier without calling it
#   LG_USE_OK    1: a `use` statement is an import, not a mention or a site (an `as` still is a mention)
#   LG_REF_USE_ONLY  1: only a `use ... as` line is reported as an alias (REF); a mention anywhere else
#                is expected to match LG_SITE too, so it is a site of its own (`.and_then(client_for)`
#                reads the registry as surely as a call does)
#   LG_KEYSPELL  1: the key carries the matched spelling, so two different getters in one fn are two lines
#   LG_BARE      `IMPORT@@SITE;;IMPORT@@SITE`: a bare spelling that is a site only in a file that imports
#                it. IMPORT is an ERE over the text of one `use` statement (joined across lines); when
#                one matches, SITE joins LG_SITE for the whole file
#   LG_FILE_SITE `PATH@@SITE;;..`: SITE joins LG_SITE in the file PATH alone (the module that defines a
#                getter calls it bare, and no `use` says so)
#   LG_REEXPORT_FILE / LG_REEXPORT_RE   the one bare mention tolerated, by file and exact line
#   -- fork: an attribute `#[cfg(..)]` or an inline `cfg!(..)` whose predicate reads `test` /
#      `feature = "test-support"`. An attribute counts only when production can satisfy it (an item no
#      production build compiles is skipped whole); an inline `cfg!(..)` counts whatever it evaluates
#      to, because `if cfg!(test) {..} else {..}` compiles an arm production runs and a test does not.
#      No variables --
#   -- arm: every arm of the match inside `fn LG_ARM_FN` whose text lacks LG_ARM_OK. One site per
#      `AppFx::<Variant>` on the arm's head (the text before its `=>`, wrapped over lines or not), so
#      `AppFx::A(r) | AppFx::B(r) => ..` is two; an arm whose head names no variant (`other => ..`,
#      `_ => ..`) is one site, `AppFx::_` --
#   LG_ARM_FN    the fn that holds the match
#   LG_ARM_MATCH ERE of the line that opens the match; its arms are the lines the body is made of
#   LG_ARM_OK    ERE; an arm that matches it is on the ordered lane and is not a site
#
# What counts as production, in every mode: comments, string and char literals (raw strings, block
# comments and multi-line strings included) are blanked before anything is matched or counted, so a
# brace or a call spelled inside a message cannot move the scan; and any item, statement or field
# under a `#[cfg(..)]` that production can never satisfy (`test`, `any(test, feature = "test-support")`,
# `all(test, ..)`) is skipped whole — not only a `mod`, a `fn` or a `let` under it is test code too.
# The enclosing fn is the innermost `fn <name>` whose body holds the site, tracked by brace depth; a
# site in a closure belongs to the fn around the closure.

function spaces(n,    s) {
  if (SPC == "") SPC = sprintf("%256s", "")
  if (n <= 256) return substr(SPC, 1, n)
  s = ""; while (n-- > 0) s = s " "; return s
}
function isident(c) { return c ~ /[A-Za-z0-9_]/ }

# lexline(line): sets LC (code: comments removed, string/char contents blanked) and LK (keep: comments
# removed, literals intact). Both stay the same length, so an offset in one is an offset in the other.
# The string / raw-string / block-comment state carries across lines. Between two points where the state
# can change (a comment or a literal opening or closing) a stretch of text is copied or blanked in one
# piece: this is the scanner's whole cost, and a character at a time made it the slowest part of `make check`'s
# dependency gate.
function lexline(line,    n, i, c, d, oc, ok, j, h, k, m, p, pp, rest, w) {
  if (inblock == 0 && instr == 0 && line !~ /["'\/]/) { LC = line; LK = line; return }
  n = length(line); i = 1; oc = ""; ok = ""
  while (i <= n) {
    rest = substr(line, i)
    if (inblock > 0) {
      w = match(rest, /[\/*]/)
      if (w == 0) break
      i += w - 1
      d = substr(line, i, 2)
      if (d == "*/") { inblock--; i += 2 } else if (d == "/*") { inblock++; i += 2 } else i++
      continue
    }
    if (instr == 1) {
      w = match(rest, /["\\]/)
      if (w == 0) { oc = oc spaces(length(rest)); ok = ok rest; break }
      if (w > 1) { oc = oc spaces(w - 1); ok = ok substr(rest, 1, w - 1); i += w - 1 }
      c = substr(line, i, 1)
      if (c == "\\") { m = substr(line, i, 2); oc = oc spaces(length(m)); ok = ok m; i += 2; continue }
      instr = 0; oc = oc c; ok = ok c; i++; continue
    }
    if (instr == 2) {
      w = index(rest, "\"")
      if (w == 0) { oc = oc spaces(length(rest)); ok = ok rest; break }
      if (w > 1) { oc = oc spaces(w - 1); ok = ok substr(rest, 1, w - 1); i += w - 1 }
      if (substr(line, i, length(rawclose)) == rawclose) {
        oc = oc rawclose; ok = ok rawclose; i += length(rawclose); instr = 0; continue
      }
      oc = oc " "; ok = ok "\""; i++; continue
    }
    w = match(rest, /["'\/]|r#*"/)
    if (w == 0) { oc = oc rest; ok = ok rest; break }
    if (w > 1) { m = substr(rest, 1, w - 1); oc = oc m; ok = ok m; i += w - 1 }
    c = substr(line, i, 1)
    d = substr(line, i, 2)
    if (d == "//") break
    if (d == "/*") { inblock = 1; i += 2; continue }
    if (c == "\"") { instr = 1; oc = oc c; ok = ok c; i++; continue }
    if (c == "r") {
      p = (i > 1) ? substr(line, i - 1, 1) : ""; pp = (i > 2) ? substr(line, i - 2, 1) : ""
      if (!isident(p) || ((p == "b" || p == "c") && !isident(pp))) {
        j = i + 1; h = ""
        while (substr(line, j, 1) == "#") { h = h "#"; j++ }
        if (substr(line, j, 1) == "\"") {
          rawclose = "\"" h; instr = 2
          m = substr(line, i, j - i + 1); oc = oc m; ok = ok m; i = j + 1; continue
        }
      }
    }
    if (c == "'") {
      if (substr(line, i + 1, 1) == "\\") {
        k = index(substr(line, i + 3), "'")
        if (k > 0) { m = k + 3; oc = oc spaces(m); ok = ok substr(line, i, m); i += m; continue }
      } else if (substr(line, i + 2, 1) == "'" && substr(line, i + 1, 1) != "'") {
        oc = oc "   "; ok = ok substr(line, i, 3); i += 3; continue
      }
    }
    oc = oc c; ok = ok c; i++
  }
  LC = oc; LK = ok
}

# count(s, ch): how many ch (a single character) in s
function count(s, ch,    t) { t = s; return gsub(ch, "", t) }
function balance(s,    t, o, c) {
  t = s; o = gsub(/[(\[{]/, "", t); t = s; c = gsub(/[)\]}]/, "", t); return o - c
}

# --- cfg predicate evaluation: 0 false, 1 true, 2 unknown. `test` and `feature = "test-support"` take
# the value `tmode` (0 = production, 1 = test); every other atom (target_os, devtriggers, hostsim ..) is unknown.
function tokenize(s,    n, i, c, j) {
  ntok = 0; n = length(s); i = 1
  while (i <= n) {
    c = substr(s, i, 1)
    if (c ~ /[A-Za-z_]/) {
      j = i; while (j <= n && substr(s, j, 1) ~ /[A-Za-z0-9_]/) j++
      tok[++ntok] = substr(s, i, j - i); i = j
    } else if (c == "\"") {
      j = index(substr(s, i + 1), "\""); if (j == 0) j = n - i
      tok[++ntok] = substr(s, i, j + 1); i += j + 1
    } else if (c == "(" || c == ")" || c == "," || c == "=") { tok[++ntok] = c; i++ }
    else i++
  }
}
function ev(tmode,    name, v, r, val) {
  name = tok[tp++]
  if (tok[tp] == "(") {
    tp++
    r = (name == "any") ? 0 : 1
    while (tp <= ntok && tok[tp] != ")") {
      v = ev(tmode)
      if (name == "any") { if (v == 1) r = 1; else if (v == 2 && r == 0) r = 2 }
      else if (name == "not") r = (v == 2) ? 2 : 1 - v
      else { if (v == 0) r = 0; else if (v == 2 && r == 1) r = 2 }
      if (tok[tp] == ",") tp++
    }
    tp++
    return r
  }
  if (tok[tp] == "=") {
    tp++; val = tok[tp++]
    if (name == "feature" && val == "\"test-support\"") { sawtest = 1; return tmode }
    return 2
  }
  if (name == "test") { sawtest = 1; return tmode }
  return 2
}
# eval_cfg(text, tmode): value of `cfg(..)` / `cfg!(..)` text, or -1 when it is not a cfg at all
function eval_cfg(text, tmode,    v) {
  tokenize(text)
  if (tok[1] != "cfg") return -1
  tp = 1; sawtest = 0
  v = ev(tmode)
  return v
}

# the name of the item a line opens, or "" when it opens none (a statement, a field, an arm)
function itemname(code,    s) {
  if (match(code, /(^|[^A-Za-z0-9_])fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*/)) {
    s = substr(code, RSTART, RLENGTH); sub(/^.*fn[[:space:]]+/, "", s); return s
  }
  if (match(code, /(^|[^A-Za-z0-9_])static[[:space:]]+mut[[:space:]]+[A-Za-z_][A-Za-z0-9_]*/)) {
    s = substr(code, RSTART, RLENGTH); sub(/^.*mut[[:space:]]+/, "", s); return s
  }
  if (match(code, /(^|[^A-Za-z0-9_])(mod|static|const|struct|enum|union|trait|type)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*/)) {
    s = substr(code, RSTART, RLENGTH); sub(/^[^A-Za-z0-9_]*[A-Za-z0-9_]+[[:space:]]+/, "", s); return s
  }
  if (code ~ /(^|[^A-Za-z0-9_])impl[[:space:]<]/) {
    s = code; sub(/^.*(^|[^A-Za-z0-9_])impl/, "impl", s); sub(/[[:space:]]*(where.*|\{.*)?$/, "", s)
    gsub(/[[:space:]]+/, " ", s); return s
  }
  if (code ~ /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]/) {
    s = code; sub(/^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]+/, "use ", s)
    sub(/[[:space:]]*[;{].*$/, "", s); return s
  }
  # a macro invocation outside every fn is an item named for its macro, qualified or not
  # (`plx_base::devtrig::latched_flag!(..);` is `latched_flag!`); inside a fn it is a statement
  if (top == 0 && match(code, /^[[:space:]]*(::)?([A-Za-z_][A-Za-z0-9_]*::)*[A-Za-z_][A-Za-z0-9_]*![[:space:]]*[({]/)) {
    s = substr(code, RSTART, RLENGTH); sub(/^[[:space:]]*/, "", s); sub(/[[:space:]]*[({]$/, "", s)
    sub(/^.*::/, "", s); return s
  }
  return ""
}

# the name of the struct / enum / union / trait / impl a line opens, or "" (`impl` first: an `impl<const N: usize>`
# header would otherwise read as the `const` item `N`)
function contname(code,    s) {
  if (code ~ /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?((unsafe|default)[[:space:]]+)*impl[[:space:]<]/) {
    s = code; sub(/^.*(^|[^A-Za-z0-9_])impl/, "impl", s); sub(/[[:space:]]*(where.*|\{.*)?$/, "", s)
    gsub(/[[:space:]]+/, " ", s); return s
  }
  return itemname(code)
}

# the fn around the line; outside every fn, the container (a struct, enum, union, trait or impl) around it --
# a field under a `#[cfg(..)]` has no fn, and its container is the only name it has; `?` when it has neither
function enclosing() { return (top > 0) ? stack[top] : ((pending != "") ? pending : ((ctop > 0) ? cstack[ctop] : "?")) }
function pop_to(d) { while (top > 0 && sdepth[top] > d) top--; while (ctop > 0 && csdepth[ctop] > d) ctop-- }

function emit_calls(code, hit,    rest, m) {
  rest = code
  while (match(rest, SITE)) {
    if (KEYSPELL != "") {
      m = substr(rest, RSTART, RLENGTH)
      gsub(/^[^A-Za-z0-9_]+/, "", m); gsub(/[^A-Za-z0-9_]+$/, "", m)
      print file "::" hit "::" m
    } else print file "::" hit
    # resume at the match's last character (it may be the leading character of the next site); a
    # one-character match must still move on
    rest = substr(rest, RLENGTH > 1 ? RSTART + RLENGTH - 1 : RSTART + 1)
  }
}

# a gathered attribute starting at line i: sets AT (its kept text), AEND (its last line) and APOS (the
# offset in that line just past its closing `]`)
function gather_attr(i,    j, bal, s, k, ch, t, line, started) {
  AT = ""; bal = 0; started = 0
  for (j = i; j <= N; j++) {
    line = C[j]; t = ""
    for (k = 1; k <= length(line); k++) {
      ch = substr(line, k, 1)
      if (!started) { if (ch != "#") continue; started = 1 }
      t = t substr(K[j], k, 1)
      if (ch == "[") bal++
      else if (ch == "]") { bal--; if (bal <= 0) { AT = AT t; AEND = j; APOS = k + 1; return } }
    }
    AT = AT t " "
  }
  AEND = N; APOS = length(C[N]) + 1
}

# the text of a `cfg!(..)` call whose `cfg!` starts at offset p of line i (up to 8 lines)
function gather_cfg_macro(i, p,    j, s, k, ch, bal, t, line) {
  t = ""; bal = 0
  for (j = i; j <= N && j < i + 8; j++) {
    line = K[j]
    for (k = (j == i ? p : 1); k <= length(line); k++) {
      ch = substr(line, k, 1); t = t ch
      if (ch == "(") bal++
      else if (ch == ")") { bal--; if (bal <= 0) return t }
    }
    t = t " "
  }
  return t
}

# the arm in hand ends: one site per `AppFx::<Variant>` on its head, or `AppFx::_` when it names none,
# unless it rides the ordered lane
function flush_arm(    n, v) {
  if (inarm && !armok) {
    n = 0; v = armhead
    while (match(v, /AppFx::[A-Za-z0-9_]+/)) { print file "::" substr(v, RSTART, RLENGTH); v = substr(v, RSTART + RLENGTH); n++ }
    if (n == 0) print file "::AppFx::_"
  }
  inarm = 0; armhead = ""; armok = 0
}

# one line of the match body of LG_ARM_FN (armdepth > 0 while it is open). An arm starts on the first
# line after the previous one ended, and ends on the first line that leaves every bracket balanced and
# ends in `,` or `}` once its `=>` has been read; its head is the text before that `=>`. The match's own
# closing brace is a `}` at the body's depth that follows a finished arm.
function arm_line(code,    p) {
  if (depth == armdepth && abal == 0 && code ~ /^[[:space:]]*\}/) { flush_arm(); armdepth = -1; return }
  if (!inarm) { inarm = 1; armhead = ""; headdone = 0; armok = 0; abal = 0 }
  if (!headdone) {
    p = index(code, "=>")
    if (p > 0) { armhead = armhead " " substr(code, 1, p - 1); headdone = 1 } else armhead = armhead " " code
  }
  if (code ~ ARMOK) armok = 1
  abal += balance(code)
  if (headdone && abal == 0 && code ~ /[,}][[:space:]]*$/) flush_arm()
}

# widen SITE with the spellings this file alone makes sites: its own path's, and the bare names it
# imports (a `use` statement is read whole, so a group split over lines counts)
function widen_site(    i, code, stmt, k) {
  for (k = 1; k <= NFS; k++) if (file == FSP[k]) SITE = SITE "|" FSS[k]
  if (NBARE == 0) return
  for (k = 1; k <= NBARE; k++) BON[k] = 0
  stmt = ""
  for (i = 1; i <= N; i++) {
    code = C[i]
    if (stmt == "" && code !~ /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]/) continue
    stmt = stmt " " code
    if (code ~ /;/) {
      for (k = 1; k <= NBARE; k++) if (!BON[k] && stmt ~ BIM[k]) { BON[k] = 1; SITE = SITE "|" BSI[k] }
      stmt = ""
    }
  }
}

function scan_file(    i, code, scan, uline, name, k, hit, rest, p, txt, v, n, o, c, v0, v1, nf) {
  depth = 0; top = 0; pending = ""; skipping = 0; pendfork = 0; inuse = 0; ctop = 0; cpending = ""
  inarm = 0; armhead = ""; armok = 0; armdepth = 0; armopen = 0; abal = 0; headdone = 0
  if (MODE == "call") widen_site()
  for (i = 1; i <= N; i++) {
    code = C[i]
    if (skipping) {
      # what is skipped ends where its item does. The item's kind is read from its first line that is not
      # an attribute (an attribute is balanced, so sbal is 0 there): an item (`fn`, `impl`, `mod`, ..)
      # runs to the `;` that ends it before any `{`, or to the close of its first `{`..`}` -- a signature
      # that spills over lines, or a `where` clause whose bounds end in a comma, is not its end; a field,
      # an arm, a variant or a statement ends at the first balanced `,`, `;` or `}`
      if (skind == "" && sbal == 0 && code !~ /^[[:space:]]*#/) skind = (code ~ ITEMKW) ? "item" : "plain"
      if (code ~ /\{/) sopen = 1
      sbal += balance(code)
      if (sbal <= 0 && (skind == "item" ? (sopen ? code ~ /[;}][[:space:]]*$/ : code ~ /;[[:space:]]*$/) \
                                        : code ~ /[;},][[:space:]]*$/)) skipping = 0
      continue
    }
    if (code ~ /^[[:space:]]*#!?\[/) {
      gather_attr(i)
      v0 = eval_cfg(AT, 0)
      if (v0 >= 0) {
        sawtest0 = sawtest
        if (v0 == 0) { skipping = 1; sbal = 0; skind = ""; sopen = 0 }
        else if (MODE == "fork" && sawtest0) pendfork++
      }
      rest = substr(C[AEND], APOS)
      if (rest ~ /[^[:space:]]/) { C[AEND] = rest; K[AEND] = substr(K[AEND], APOS); i = AEND - 1 }
      else i = AEND
      continue
    }
    if (code !~ /[^[:space:]]/) continue

    if (pendfork > 0) {
      name = itemname(code); if (name == "") name = enclosing()
      for (k = 0; k < pendfork; k++) print file "::" name
      pendfork = 0
    }

    # `use` statements
    uline = 0
    if (USEOK != "" && (inuse || code ~ /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]/)) { uline = 1; inuse = (code !~ /;/) }

    if (match(code, /(^|[^A-Za-z0-9_])fn[[:space:]]+[A-Za-z0-9_]+/)) {
      name = substr(code, RSTART, RLENGTH); sub(/.*fn[[:space:]]+/, "", name); pending = name
    }
    if (MODE == "fork" && cpending == "" && code ~ CONTSTART) cpending = contname(code)

    if (MODE == "call") {
      # the definition's own header is neither a site nor a mention; the rest of its line still is
      scan = code; if (DEF != "") gsub(DEF, " ", scan)
      if (REF != "" && scan ~ REF \
          && !(REXF != "" && file == REXF && code ~ REXR) \
          && !(uline && code !~ ASRE) \
          && (REFUSEONLY == "" || (uline && code ~ ASRE)))
        print "REF\t" file ":" i
      if (!uline && scan ~ SITE) {
        hit = (top > 0 ? stack[top] : (pending != "" ? pending : "?"))
        # a call on the same line as the `fn` it sits in, before that fn pushes its body
        if (pending != "" && code ~ /fn[[:space:]]/ && code ~ /\{/) hit = pending
        emit_calls(scan, hit)
      }
    } else if (MODE == "fork") {
      p = index(code, "cfg!(")
      while (p > 0) {
        txt = gather_cfg_macro(i, p)
        v = eval_cfg(txt, 0)
        if (v >= 0 && sawtest) print file "::" enclosing()
        n = index(substr(code, p + 5), "cfg!(")
        p = (n > 0) ? p + 5 + n - 1 : 0
      }
    } else if (MODE == "arm") {
      if (armdepth == 0 && top > 0 && stack[top] == ARMFN && code ~ ARMMATCH) armopen = 1
      else if (armdepth > 0) arm_line(code)
    }

    o = gsub(/\{/, "{", code); c = gsub(/\}/, "}", code)
    for (k = 0; k < o; k++) {
      depth++
      if (cpending != "") { ctop++; cstack[ctop] = cpending; csdepth[ctop] = depth; cpending = "" }
      else if (pending != "") { top++; stack[top] = pending; sdepth[top] = depth; pending = "" }
    }
    for (k = 0; k < c; k++) { depth--; pop_to(depth) }
    if (pending != "" && code ~ /;[[:space:]]*$/) pending = ""
    if (cpending != "" && code ~ /;[[:space:]]*$/) cpending = ""
    if (armopen) { armdepth = depth; armopen = 0 }
  }
  flush_arm()
}

BEGIN {
  MODE = ENVIRON["LG_MODE"]; if (MODE == "") MODE = "call"
  SITE = ENVIRON["LG_SITE"]; DEF = ENVIRON["LG_DEF"]; REF = ENVIRON["LG_REF"]
  USEOK = ENVIRON["LG_USE_OK"]; KEYSPELL = ENVIRON["LG_KEYSPELL"]
  REXF = ENVIRON["LG_REEXPORT_FILE"]; REXR = ENVIRON["LG_REEXPORT_RE"]
  ARMFN = ENVIRON["LG_ARM_FN"]; ARMMATCH = ENVIRON["LG_ARM_MATCH"]; ARMOK = ENVIRON["LG_ARM_OK"]
  # the first line of an item: its keyword, after a visibility and any qualifiers
  ITEMKW = "^[[:space:]]*(pub(\\([^)]*\\))?[[:space:]]+)?((unsafe|async|default|const|extern([[:space:]]+\"[^\"]*\")?)[[:space:]]+)*(fn|impl|mod|trait|struct|enum|union|type|static|const|use|extern|macro_rules!)([^A-Za-z0-9_]|$)"
  # the first line of a container: its keyword, after a visibility and any qualifiers
  CONTSTART = "^[[:space:]]*(pub(\\([^)]*\\))?[[:space:]]+)?((unsafe|default)[[:space:]]+)*(struct|enum|union|trait|impl)([^A-Za-z0-9_]|$)"
  REFUSEONLY = ENVIRON["LG_REF_USE_ONLY"]
  ASRE = "(^|[^A-Za-z0-9_])as[[:space:]]"
  NBARE = split_pairs(ENVIRON["LG_BARE"], BIM, BSI)
  NFS = split_pairs(ENVIRON["LG_FILE_SITE"], FSP, FSS)
  inblock = 0; instr = 0; N = 0; file = ""; SITE0 = SITE
  # an empty site pattern matches everywhere (and `match` would never advance): refuse it
  if (MODE == "call" && SITE == "") { print "ledger.awk: LG_SITE is empty" > "/dev/stderr"; BAD = 1 }
}
# split_pairs(spec, left, right): `a@@b;;c@@d` into left[1..n], right[1..n]; returns n
function split_pairs(spec, left, right,    parts, n, i, kv) {
  if (spec == "") return 0
  n = split(spec, parts, ";;")
  for (i = 1; i <= n; i++) { split(parts[i], kv, "@@"); left[i] = kv[1]; right[i] = kv[2] }
  return n
}
BAD { next }
# many files in one process (an awk start costs more than lexing a typical file): each file's lines are
# scanned when the next file starts, so no state crosses a file boundary
FNR == 1 {
  if (file != "") scan_file()
  file = FILENAME; N = 0; inblock = 0; instr = 0; SITE = SITE0
}
{ lexline($0); N++; C[N] = LC; K[N] = LK }
END { if (BAD) exit 2; if (file != "") scan_file() }
