#!/usr/bin/env python3
"""Phase 0: is the actuator ladder physically climbable, before any controller exists?

WHY THIS TOOL EXISTS. Two ABR guards have now been written that the top of the ladder
cannot satisfy: the shipped `buffered >= 3 * segment` (6 000 ms against a 5 421 ms
ceiling), and its proposed replacement. Both were derived carefully and both were
unsatisfiable for the same reason, which is not a control-law reason at all:

    an upshift guard is Omega(D)                 -- it must cover a transaction
    B_max(R) = lead + queue_bits / R_ES          -- and the ceiling falls as 1/R

They cross. Above the crossing rate no guard of that shape can be satisfied, so the
question "which guard" is downstream of the question "is this ladder climbable on this
queue" -- and that second question is answered here, on the host, in a second, with no
television and no controller.

THREE CEILING CONDITIONS, not one. A rung is usable only if its reachable reserve covers
all three; earlier passes checked only the first and called the ladder fixed.

    B_max(R_j) >= up-guard(j-1 -> j)        can the rung be REACHED from below
    B_max(R_j) >= A_j + E_tx_down(k*)       can ONE wrong admission be SURVIVED
    B_max(R_j) >= D                         does ONE SEGMENT even fit   (else aq_push
                                            blocks forever: a silent hang, not a stall)

AGAINST THE LOW END OF THE MODEL, NOT THE MODEL. `b_max_est` is a model whose device
error spans -1.6%..+2.7%. At rung 16000 the model says 6 064 ms and the device says
5 960 against a 6 000 ms threshold -- opposite sides of the decision. A sweep run on the
model alone certifies a guard the television cannot satisfy, so measured p10 is used
where it exists and the model is discounted where it does not.

Prints a matrix over the candidate plant configurations. Exit status is advisory only.
"""
import argparse, json, sys

D_MS = 2000                       # EXT-X-TARGETDURATION honoured on every corpus sample
AQ_VIDEO_BYTES = 8 * 1024 * 1024  # player/engine.rs:91
AQ_AUDIO_BYTES = 1 * 1024 * 1024  # player/engine.rs:92
FEED_AHEAD_MS  = 1600             # player/engine.rs:1051  MAX_FEED_AHEAD_NS
AUDIO_SLACK_MS = 2000             # player/engine.rs:1052  AUDIO_SLACK_NS
TS_OVERHEAD    = 1.04             # ASSUMPTION, not a measurement (sim.rs:246-250)

# Delivered TS rate per rung, measured off `abr: sample media=` in the corpus.
# Rungs 12000 and 22000 never ran: no fixture clip, so no measurement.
MEDIA_KBPS = {320: 514, 720: 1381, 2000: 3183, 4000: 3183, 6000: 7097, 8000: 9265,
              10000: 11147, 14000: 14486, 16000: 15824, 18000: 17376, 20000: 18456}
AUDIO_KBPS = {720: 131, 4000: 160, 20000: 192}   # ffprobe; others assumed
AUDIO_ASSUMED = 192

# Observed reserve, settled leg, first quarter discarded (i1-abr-baseline.md).
OBSERVED_P10 = {720: 50085, 4000: 24751, 20000: 5293}
MODEL_DISCOUNT = 0.984            # worst over-prediction seen: 5335/5421

# E_tx_down medians per landing rung, from the 17 committed down-legs in i2-logs.
#
# A median summarises this honestly only because a downshift now carries a DEADLINE (J3b). Before
# it, `E_tx_down` was bimodal — p95 2 198 ms against a max of 36 164 — because the fail-safe
# transaction had no bound of any kind, so no quantile of it described a quantity. The deadline
# caps the transfer at the reserve being spent, which is what makes the tail these medians omit a
# bounded tail rather than an open one. The medians themselves are unchanged: every one of them is
# far under any reserve the ladder holds, so the deadline does not move them.
E_TX_DOWN_MS = {320: 295, 720: 725, 2000: 1783, 4000: 1356, 6000: 967,
                8000: 773, 14000: 1209, 16000: 1305, 18000: 1445}
E_TX_DOWN_FALLBACK = 1800         # worst measured; used where a rung never landed

O0_MS = 18                        # A = O0 + bytes*tau; intercept, cluster-CI [14, 21]
CONTROL_MS = 6                    # prime + master + media playlist, fixture tier
COLD_START_MS = 250               # encoder cold start; UNMEASURED off-fixture


def es_rates(rung):
    ts = MEDIA_KBPS[rung]
    audio = AUDIO_KBPS.get(rung, AUDIO_ASSUMED)
    return max(1, int((ts - audio) / TS_OVERHEAD)), max(1, audio)


def b_max_ms(rung, aq_video, aq_audio, lead_ms, audio_slack_ms):
    """bits / kbps is ALREADY ms -- the kbit form yields seconds and is wrong by 1000x."""
    v_es, a_es = es_rates(rung)
    video = lead_ms + (aq_video * 8) // v_es
    audio = lead_ms + audio_slack_ms + (aq_audio * 8) // a_es
    return min(video, audio)


def b_max_floor(rung, cfg):
    """Lower bound on the TRUE reserve: measured p10 where we have it, discounted model elsewhere."""
    model = b_max_ms(rung, cfg["aq_video"], cfg["aq_audio"], cfg["lead"], cfg["audio_slack"])
    if rung in OBSERVED_P10 and cfg["aq_video"] == AQ_VIDEO_BYTES and cfg["lead"] == FEED_AHEAD_MS:
        return min(model, OBSERVED_P10[rung]), "measured p10"
    return int(model * MODEL_DISCOUNT), "model x0.984"


def up_guard(a_i, a_j, e_tx_down, graded_deadline, reject_delivers):
    """Reserve an upshift proposal must have.

    reject_delivers=False (shipped): a reject returns NOTHING, so the guard must cover the
    whole transaction plus one observable control step.
    reject_delivers=True: feeding the completed warm-up segment on a graded reject turns a
    reject into a one-segment excursion, so the transaction is repaid by D and the guard
    covers the excursion plus the trip back instead.
    """
    if reject_delivers:
        return CONTROL_MS + a_j + COLD_START_MS + e_tx_down + a_i
    return CONTROL_MS + a_j + COLD_START_MS + graded_deadline + a_i


def sweep(cfg, ladder, reject_delivers, verbose=False):
    """Worst case over (A_i, A_j) in [O0, D]^2 -- both are admissible anywhere in that box."""
    rows, ok_all = [], True
    for idx in range(1, len(ladder)):
        prev, rung = ladder[idx - 1], ladder[idx]
        floor, prov = b_max_floor(rung, cfg)
        e_down = E_TX_DOWN_MS.get(prev, E_TX_DOWN_FALLBACK)
        worst_reach = max(up_guard(a_i, a_j, e_down, D_MS, reject_delivers)
                          for a_i in (O0_MS, D_MS) for a_j in (O0_MS, D_MS))
        survive = D_MS + E_TX_DOWN_MS.get(rung, E_TX_DOWN_FALLBACK)
        need = max(worst_reach, survive, D_MS)
        ok = floor >= need
        ok_all &= ok
        rows.append({"from": prev, "to": rung, "b_max_floor": floor, "provenance": prov,
                     "reach": worst_reach, "survive": survive, "fits": D_MS,
                     "need": need, "margin": floor - need, "ok": ok})
    return rows, ok_all


CONFIGS = [
    ("shipped",                dict(aq_video=AQ_VIDEO_BYTES,      aq_audio=AQ_AUDIO_BYTES, lead=FEED_AHEAD_MS, audio_slack=AUDIO_SLACK_MS)),
    ("aq 10 MiB",              dict(aq_video=10 * 1024 * 1024,    aq_audio=AQ_AUDIO_BYTES, lead=FEED_AHEAD_MS, audio_slack=AUDIO_SLACK_MS)),
    ("aq 12 MiB",              dict(aq_video=12 * 1024 * 1024,    aq_audio=AQ_AUDIO_BYTES, lead=FEED_AHEAD_MS, audio_slack=AUDIO_SLACK_MS)),
    ("lead 3.8 s",             dict(aq_video=AQ_VIDEO_BYTES,      aq_audio=AQ_AUDIO_BYTES, lead=3800,          audio_slack=AUDIO_SLACK_MS)),
]

LADDERS = {
    "full":     [320, 720, 2000, 4000, 6000, 8000, 10000, 14000, 16000, 18000, 20000],
    "no-ties":  [320, 720, 2000, 4000, 6000, 8000, 10000, 14000, 18000],
}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ladder", choices=sorted(LADDERS), default="full")
    ap.add_argument("--reject-delivers", action="store_true",
                    help="model the graded-reject warm-up feed (ff.rs:3523 drops it today)")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--verbose", action="store_true", help="per-transition detail")
    args = ap.parse_args()

    ladder = LADDERS[args.ladder]
    out = {}
    for name, cfg in CONFIGS:
        rows, ok = sweep(cfg, ladder, args.reject_delivers)
        out[name] = {"climbable": ok, "rows": rows}

    if args.json:
        print(json.dumps({"ladder": args.ladder, "reject_delivers": args.reject_delivers,
                          "configs": out}, indent=2))
        return 0

    print(f"ladder = {args.ladder} ({len(ladder)} rungs)   "
          f"reject_delivers = {args.reject_delivers}   D = {D_MS} ms")
    print(f"guard = control({CONTROL_MS}) + A_j + cold({COLD_START_MS}) + "
          f"{'E_tx_down' if args.reject_delivers else 'graded_dl'} + A_i, "
          f"worst over (A_i,A_j) in [{O0_MS},{D_MS}]^2\n")
    for name, res in out.items():
        worst = min(res["rows"], key=lambda r: r["margin"])
        verdict = "CLIMBABLE" if res["climbable"] else \
                  f"BLOCKED at {worst['from']}->{worst['to']} by {-worst['margin']} ms"
        print(f"  {name:<12} {verdict}")
        if args.verbose or not res["climbable"]:
            for r in res["rows"]:
                mark = "ok  " if r["ok"] else "FAIL"
                print(f"      {mark} {r['from']:>5}->{r['to']:<5} floor {r['b_max_floor']:>6} "
                      f"({r['provenance']:<12}) need {r['need']:>5} "
                      f"[reach {r['reach']} survive {r['survive']}] margin {r['margin']:>+6}")
    print("\n  A_j at the box corner D is admissible BY DEFINITION (sustainable <=> A <= D),")
    print("  so a configuration that passes only at typical A/D is not passing.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
