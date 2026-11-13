----------------------- MODULE JunoTimerqNextDeadline -----------------------
(***************************************************************************)
(* The lock-free cached next deadline of the Juno timer queue              *)
(* (docs/design/juno-timerq.md, sections 5 and 12; kernel patch 1027), on   *)
(* top of the idle wake-up protocol of ../idle-wake/JunoIdleWake.tla.       *)
(*                                                                           *)
(* Kept from JunoIdleWake, action for action: halt, stop_tick_for_idle +    *)
(* choose_wakeup + lucina_idle::{covered, wake_at} under WAKE_LOCK, WFI,     *)
(* wake_from_idle (TICKING published before interrupts are on),             *)
(* resume_ticks, the IDLE_TIMER_WAKEUPS gate, MAX_IDLE_SLEEP (CAP), the      *)
(* tick interrupt, device interrupts, the timer softirq and its global      *)
(* RUNNING skip.                                                             *)
(*                                                                           *)
(* Replaced: the one watchdog timer by a set of timers that any running CPU *)
(* arms, re-arms in place and cancels; next_event reads the cache, not the  *)
(* queue. Left out: the scheduler, need_preempt and IPIs (they deliver a    *)
(* woken thread; JunoIdleWake S1 and S5 are about them, not about expiry).  *)
(* A CPU that leaves an interrupt either runs a thread ("run") or idles,    *)
(* nondeterministically.                                                     *)
(*                                                                           *)
(* Steps that only one CPU can see are joined to the step before them (the  *)
(* interrupt entry with wake_from_idle and the tick handler; the return     *)
(* from the interrupt with halt and stop_tick): the reachable states where  *)
(* every CPU waits, and all that another CPU can read, are the same.        *)
(*                                                                           *)
(* The weak-memory rule of section 12, exactly:                             *)
(*   hist      the values stored to next_deadline, in the order of the      *)
(*             manager lock; each is the root after its operation           *)
(*   seen[c]   the newest store CPU c is guaranteed to observe (its own     *)
(*             stores)                                                       *)
(*   lockSeen  what the last WAKE_LOCK holder had seen at its release       *)
(* A load in StopTick returns hist[k] for ANY k with                        *)
(* Max(seen[c], lockSeen) <= k <= Len(hist).                                 *)
(* hist is kept from the oldest index still readable (Mem): indices below   *)
(* every seen[c] and lockSeen can never be read again.                      *)
(*                                                                           *)
(* Mutant is "none" for the design, or names a deliberate change.           *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS CPUS,      \* CPU numbers
          TIMERS,    \* timer identities (slots)
          CAP,       \* MAX_IDLE_SLEEP (1 s) in ticks, scaled down
          TMAX,      \* how far time runs in the search
          DEVIRQS,   \* how many device interrupts may come in a run
          ARMS,      \* how many arm or re-arm operations in a run
          CANCELS,   \* how many cancel operations in a run
          DELTAS,    \* an arm sets the deadline to now + d, d in DELTAS
          BATCH,     \* timers popped by one softirq run (B x K of section 3); the rest re-raises
          EXP_BOUND, \* ExpiryInTime: an armed timer is fired within this many ticks of its deadline:
                     \* 1 while no run leaves due timers behind, else ceil(due / BATCH) (section 12,
                     \* "The expiry bound"); softirqd, after 5 re-runs, is not modelled
          Mutant

ASSUME CAP >= 2 /\ TMAX >= 1 /\ BATCH >= 1

TICKING == 0                \* lucina_idle::TICKING
ABSENT  == TMAX + CAP + 100 \* lucina_idle::ABSENT: later than any time here
INF     == TMAX + CAP + 200 \* u64::MAX in the cache: the queue is empty
NONE    == -1

Min(a, b) == IF a < b THEN a ELSE b
Max(a, b) == IF a > b THEN a ELSE b
MinSet(S) == CHOOSE x \in S : \A y \in S : x <= y

\* CPUs and timers are interchangeable (no CPU is special here): the configurations make them
\* model values and give this as SYMMETRY.
Sym == Permutations(CPUS) \cup Permutations(TIMERS)

VARIABLES
    now,        \* the counter, in ticks
    pc,         \* per CPU: "run" (a thread, interrupts on), "irq" (interrupt context, nothing
                \* pending at the GIC), "soft" (the timer softirq's callbacks, RUNNING held), "wfi",
                \* "stop2" (mutant no_wake_lock only)
    cval,       \* per CPU: COUNTER_VAL = CNTV_CVAL, the next timer interrupt
    stopped,    \* per CPU: TICK_STOPPED
    twakes,     \* per CPU: IDLE_TIMER_WAKEUPS, saturated at 2
    wakeAt,     \* per CPU: WAKE_AT
    softP,      \* per CPU: the timer softirq raised
    devP,       \* per CPU: a device interrupt pending
    running,    \* the CPU in the timer softirq's callbacks (RUNNING), or NONE
    softAt,     \* when the RUNNING holder began its callbacks (0 while nobody holds it)
    devLeft,    \* device interrupts left in this run
    exp,        \* per timer: its deadline, or NONE while not armed (the queue, abstracted to a set)
    hist,       \* the stores to next_deadline, oldest readable first
    seen,       \* per CPU: index in hist of the newest store it must observe
    lockSeen,   \* index in hist the last WAKE_LOCK holder had seen at its release
    armsLeft, cancelsLeft,
    pend,       \* mutant store_outside_lock: per CPU, a computed root not yet stored, or NONE
    dec         \* mutant no_wake_lock: per CPU, the choice made from its reads, not yet written

vars == <<now, pc, cval, stopped, twakes, wakeAt, softP, devP, running, softAt, devLeft, exp,
          hist, seen, lockSeen, armsLeft, cancelsLeft, pend, dec>>

NoDec == [stop |-> FALSE, w |-> 0]

Armed(t)    == exp[t] # NONE
Root(e)     == IF \A t \in TIMERS : e[t] = NONE THEN INF
               ELSE MinSet({e[t] : t \in {u \in TIMERS : e[u] # NONE}})
TimerDue(c) == now >= cval[c]
Pending(c)  == TimerDue(c) \/ devP[c]

\* Time may pass only while every CPU waits for an interrupt: in WFI, or running a thread with
\* interrupts on, or (one tick boundary only) in the softirq's callbacks with RUNNING held: the
\* window of the RUNNING skip.
Waiting(c) == /\ pend[c] = NONE
              /\ \/ pc[c] \in {"wfi", "run"} /\ ~Pending(c)
                 \/ pc[c] = "soft" /\ softAt = now

\* The mutants that take the WAKE_LOCK happens-before away.
UsesLockSeen == Mutant \notin {"no_lock_seen", "no_wake_lock"}

(* The memory state after an update, re-based on the oldest index still readable. *)
Mem(h, sn, ls) ==
    LET pts  == {sn[c] : c \in CPUS} \cup (IF UsesLockSeen THEN {ls} ELSE {})
        base == IF Mutant = "own_store_stale" THEN 1 ELSE MinSet(pts)
    IN [h  |-> SubSeq(h, base, Len(h)),
        sn |-> [c \in CPUS |-> sn[c] - base + 1],
        ls |-> IF UsesLockSeen THEN ls - base + 1 ELSE 1]

SetMem(m) == hist' = m.h /\ seen' = m.sn /\ lockSeen' = m.ls

(* A store of the root v under the manager lock (Release): appended, and the storing CPU has
   seen it. Mutant store_outside_lock: the value is computed now and lands later (Land). *)
Store(c, v) ==
    IF Mutant = "store_outside_lock"
    THEN /\ pend' = [pend EXCEPT ![c] = v]
         /\ UNCHANGED <<hist, seen, lockSeen>>
    ELSE /\ SetMem(Mem(Append(hist, v), [seen EXCEPT ![c] = Len(hist) + 1], lockSeen))
         /\ UNCHANGED pend

Init ==
    /\ now = 0
    /\ pc = [c \in CPUS |-> "run"]
    /\ cval = [c \in CPUS |-> 1]
    /\ stopped = [c \in CPUS |-> FALSE]
    /\ twakes = [c \in CPUS |-> 2]
    /\ wakeAt = [c \in CPUS |-> TICKING]
    /\ softP = [c \in CPUS |-> FALSE]
    /\ devP = [c \in CPUS |-> FALSE]
    /\ running = NONE
    /\ softAt = 0
    /\ devLeft = DEVIRQS
    /\ exp = [t \in TIMERS |-> NONE]
    /\ hist = <<INF>>
    /\ seen = [c \in CPUS |-> 1]
    /\ lockSeen = 1
    /\ armsLeft = ARMS
    /\ cancelsLeft = CANCELS
    /\ pend = [c \in CPUS |-> NONE]
    /\ dec = [c \in CPUS |-> NoDec]

-----------------------------------------------------------------------------
(* Arms and cancels: thread context with interrupts on ("run"), or a timer callback through its
   guard ("soft": the interval re-arm). An arm in a hard interrupt handler is not a separate
   case: for every other CPU it is an arm in "run" just before or after that interrupt. In all
   of them the CPU's wake slot is TICKING (invariant TickingWhenRunning). Mutant
   no_ticking_rule: also between choose_wakeup and the WFI. *)
MayArm(c) == \/ pc[c] \in {"run", "soft"}
             \/ Mutant = "no_ticking_rule" /\ pc[c] = "wfi"

(* set_timeout: arm, or re-key in place; the cache gets the new root. *)
Arm(c, t, d) ==
    /\ MayArm(c)
    /\ armsLeft > 0
    /\ exp[t] # now + d
    /\ exp' = [exp EXCEPT ![t] = now + d]
    /\ Store(c, Root(exp'))
    /\ armsLeft' = armsLeft - 1
    /\ UNCHANGED <<now, pc, cval, stopped, twakes, wakeAt, softP, devP, running, softAt, devLeft,
                   cancelsLeft, dec>>

(* TimerGuard::cancel: removed at once; a cancel of the root raises the cached value. *)
Cancel(c, t) ==
    /\ MayArm(c)
    /\ cancelsLeft > 0
    /\ Armed(t)
    /\ exp' = [exp EXCEPT ![t] = NONE]
    /\ Store(c, Root(exp'))
    /\ cancelsLeft' = cancelsLeft - 1
    /\ UNCHANGED <<now, pc, cval, stopped, twakes, wakeAt, softP, devP, running, softAt, devLeft,
                   armsLeft, dec>>

(* Mutant store_outside_lock: the delayed store lands, after any store made meanwhile. *)
Land(c) ==
    /\ pend[c] # NONE
    /\ SetMem(Mem(Append(hist, pend[c]), [seen EXCEPT ![c] = Len(hist) + 1], lockSeen))
    /\ pend' = [pend EXCEPT ![c] = NONE]
    /\ UNCHANGED <<now, pc, cval, stopped, twakes, wakeAt, softP, devP, running, softAt, devLeft,
                   exp, armsLeft, cancelsLeft, dec>>

(* choose_wakeup + lucina_idle::{covered, wake_at}, from the cache value v and the WAKE_AT
   table wa as read. STEP = 1. As JunoIdleWake's StopTick with Regrid = FALSE. *)
Decision(c, v, wa) ==
    LET nt   == cval[c]
        capW == IF now + CAP <= nt THEN NONE ELSE now + CAP
        ev   == IF v = INF THEN NONE ELSE Max(v - now, 0)
        keep == ev # NONE /\ ev < CAP /\ now + ev <= nt
        e    == IF ev # NONE /\ ev < CAP THEN now + ev ELSE NONE
        cov  == IF Mutant = "always_covered" THEN TRUE
                ELSE \E i \in CPUS \ {c} : wa[i] # ABSENT /\ wa[i] <= e
        w    == IF e # NONE /\ ~cov THEN Min(e, capW) ELSE capW
        st   == twakes[c] >= 2 /\ capW # NONE /\ ~keep
    IN [stop |-> st, w |-> IF st THEN w ELSE 0]

(* The writes of stop_tick_for_idle. A tick that is kept keeps its compare value (nt). *)
Apply(c, d) ==
    IF d.stop
    THEN /\ wakeAt' = [wakeAt EXCEPT ![c] = d.w]
         /\ stopped' = [stopped EXCEPT ![c] = TRUE]
         /\ cval' = [cval EXCEPT ![c] = d.w]
    ELSE UNCHANGED <<wakeAt, stopped, cval>>

\* The store indices a load in StopTick may return the value of.
ReadFrom(c) == IF UsesLockSeen THEN Max(seen[c], lockSeen) ELSE seen[c]
Readable(c) == {hist[k] : k \in (IF Mutant = "own_store_stale" THEN 1 ELSE ReadFrom(c))..Len(hist)}

\* The thread blocks, or the interrupt returns to the idle thread: halt_cpu, interrupts off.
MayIdle(c) == pc[c] = "run" \/ (pc[c] = "irq" /\ ~softP[c])

(* stop_tick_for_idle, all under WAKE_LOCK, so one step: the stale read of next_deadline, the
   choice, the WAKE_AT and compare-value writes. The lock's acquire makes the CPU see what the
   previous holder had seen; its release passes that on. *)
StopTick(c) ==
    /\ MayIdle(c)
    /\ Mutant # "no_wake_lock"
    /\ \E v \in Readable(c) : Apply(c, Decision(c, v, wakeAt))
    /\ SetMem(Mem(hist, [seen EXCEPT ![c] = ReadFrom(c)],
                  IF UsesLockSeen THEN ReadFrom(c) ELSE lockSeen))
    /\ pc' = [pc EXCEPT ![c] = "wfi"]
    /\ UNCHANGED <<now, twakes, softP, devP, running, softAt, devLeft, exp, armsLeft,
                   cancelsLeft, pend, dec>>

(* Mutant no_wake_lock: the reads (cache and WAKE_AT) and the writes are two steps, and nothing
   orders one CPU's choice against another's. *)
StopRead(c) ==
    /\ MayIdle(c)
    /\ Mutant = "no_wake_lock"
    /\ \E v \in Readable(c) : dec' = [dec EXCEPT ![c] = Decision(c, v, wakeAt)]
    /\ pc' = [pc EXCEPT ![c] = "stop2"]
    /\ UNCHANGED <<now, cval, stopped, twakes, wakeAt, softP, devP, running, softAt, devLeft,
                   exp, hist, seen, lockSeen, armsLeft, cancelsLeft, pend>>

StopWrite(c) ==
    /\ pc[c] = "stop2"
    /\ Apply(c, dec[c])
    /\ dec' = [dec EXCEPT ![c] = NoDec]
    /\ pc' = [pc EXCEPT ![c] = "wfi"]
    /\ UNCHANGED <<now, twakes, softP, devP, running, softAt, devLeft, exp, hist, seen,
                   lockSeen, armsLeft, cancelsLeft, pend>>

(* A thread runs after the interrupt: one it woke, or the one it interrupted. *)
IrqToRun(c) ==
    /\ pc[c] = "irq"
    /\ ~softP[c]
    /\ pc' = [pc EXCEPT ![c] = "run"]
    /\ UNCHANGED <<now, cval, stopped, twakes, wakeAt, softP, devP, running, softAt, devLeft,
                   exp, hist, seen, lockSeen, armsLeft, cancelsLeft, pend, dec>>

-----------------------------------------------------------------------------
(* An interrupt is taken, from WFI (which ends on any pending interrupt, masked or not) or by
   a running thread. From WFI: wake_from_idle with interrupts still off (the gate counter;
   TICKING published; resume_ticks with STEP = 1: a tick that was stopped and is woken early
   restarts on the tick after now, since STOPPED_AT_TICK <= its stop time + 1 by TickSoon).
   Then the handlers until nothing is pending: timer_callback raises the softirq and
   set_next_timer = COUNTER_VAL + STEP; the device handler. *)
Enter(c) ==
    /\ pc[c] \in {"wfi", "run"}
    /\ Pending(c)
    /\ LET idle == pc[c] = "wfi"
           byT  == now >= cval[c]
           cv1  == IF idle /\ stopped[c] /\ ~byT THEN now + 1 ELSE cval[c]
           due  == now >= cv1
       IN /\ twakes' = [twakes EXCEPT ![c] =
                          IF idle THEN (IF byT THEN Min(twakes[c] + 1, 2) ELSE 0) ELSE twakes[c]]
          /\ wakeAt' = [wakeAt EXCEPT ![c] =
                          IF idle /\ stopped[c] /\ Mutant # "stale_wake_at" THEN TICKING
                          ELSE wakeAt[c]]
          /\ stopped' = [stopped EXCEPT ![c] = IF idle THEN FALSE ELSE stopped[c]]
          /\ cval' = [cval EXCEPT ![c] = IF due THEN cv1 + 1 ELSE cv1]
          /\ softP' = [softP EXCEPT ![c] = softP[c] \/ due]
    /\ devP' = [devP EXCEPT ![c] = FALSE]
    /\ pc' = [pc EXCEPT ![c] = "irq"]
    /\ UNCHANGED <<now, running, softAt, devLeft, exp, hist, seen, lockSeen, armsLeft,
                   cancelsLeft, pend, dec>>

(* Nothing pending at the GIC: the timer softirq, if raised; skipped while RUNNING is held.
   Else RUNNING is taken and process_expired_timers pops the BATCH earliest due timers under
   the manager lock and stores the new root; due timers left over re-raise the softirq. A fire
   just removes. Taking RUNNING and the pop are one step: no other CPU's step reads RUNNING
   except its own skip, which can as well come after. *)
IrqSoft(c) ==
    /\ pc[c] = "irq"
    /\ softP[c]
    /\ IF running # NONE
       THEN /\ softP' = [softP EXCEPT ![c] = FALSE]
            /\ UNCHANGED <<running, pc, softAt, exp, hist, seen, lockSeen, pend>>
       ELSE /\ running' = c
            /\ softAt' = now
            /\ pc' = [pc EXCEPT ![c] = "soft"]
            /\ LET due == {t \in TIMERS : Armed(t) /\ exp[t] <= now}
                   n   == Min(BATCH, Cardinality(due))
               IN IF due = {}
                  THEN /\ softP' = [softP EXCEPT ![c] = FALSE]
                       /\ UNCHANGED <<exp, hist, seen, lockSeen, pend>>
                  ELSE \E S \in SUBSET due :
                         /\ Cardinality(S) = n
                         /\ \A a \in S, b \in due \ S : exp[a] <= exp[b]
                         /\ exp' = [t \in TIMERS |-> IF t \in S THEN NONE ELSE exp[t]]
                         /\ Store(c, Root(exp'))
                         /\ softP' = [softP EXCEPT ![c] = (S # due)]
    /\ UNCHANGED <<now, cval, stopped, twakes, wakeAt, devP, devLeft, armsLeft, cancelsLeft, dec>>

(* The softirq's callbacks end and RUNNING is released; a tick that came due meanwhile (the
   callbacks ran across a tick boundary) is taken at once. *)
SoftEnd(c) ==
    /\ pc[c] = "soft"
    /\ running' = NONE
    /\ softAt' = 0
    /\ cval' = [cval EXCEPT ![c] = IF TimerDue(c) THEN cval[c] + 1 ELSE cval[c]]
    /\ softP' = [softP EXCEPT ![c] = softP[c] \/ TimerDue(c)]
    /\ pc' = [pc EXCEPT ![c] = "irq"]
    /\ UNCHANGED <<now, stopped, twakes, wakeAt, devP, devLeft, exp, hist, seen, lockSeen,
                   armsLeft, cancelsLeft, pend, dec>>

(* A device raises its line on a CPU in WFI (elsewhere it changes nothing this model looks at:
   the gate counts only wake-ups from idle). *)
DevIrq(c) ==
    /\ devLeft > 0
    /\ pc[c] = "wfi"
    /\ ~devP[c]
    /\ devP' = [devP EXCEPT ![c] = TRUE]
    /\ devLeft' = devLeft - 1
    /\ UNCHANGED <<now, pc, cval, stopped, twakes, wakeAt, softP, running, softAt, exp, hist,
                   seen, lockSeen, armsLeft, cancelsLeft, pend, dec>>

Advance ==
    /\ now < TMAX
    /\ \A c \in CPUS : Waiting(c)
    /\ now' = now + 1
    /\ UNCHANGED <<pc, cval, stopped, twakes, wakeAt, softP, devP, running, softAt, devLeft,
                   exp, hist, seen, lockSeen, armsLeft, cancelsLeft, pend, dec>>

(* The search ends at TMAX; this is not a deadlock. *)
End ==
    /\ now = TMAX
    /\ \A c \in CPUS : Waiting(c)
    /\ UNCHANGED vars

\* A CPU whose store has not landed (mutant store_outside_lock) is still inside that operation.
CpuStep(c) ==
    IF pend[c] # NONE THEN Land(c)
    ELSE \/ StopTick(c) \/ StopRead(c) \/ StopWrite(c) \/ IrqToRun(c) \/ Enter(c)
         \/ IrqSoft(c) \/ SoftEnd(c) \/ DevIrq(c)
         \/ (\E t \in TIMERS : Cancel(c, t) \/ (\E d \in DELTAS : Arm(c, t, d)))

Next == Advance \/ End \/ (\E c \in CPUS : CpuStep(c))

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties. *)

TypeOK ==
    /\ now \in 0..TMAX
    /\ \A c \in CPUS : pc[c] \in {"run", "irq", "soft", "wfi", "stop2"}
    /\ running \in CPUS \cup {NONE}
    /\ \A c \in CPUS : seen[c] \in 1..Len(hist)
    /\ lockSeen \in 1..Len(hist)

(* The newest store is the root of the queue (stores are serialised by the manager lock). *)
CacheIsRoot == (\A c \in CPUS : pend[c] = NONE) => hist[Len(hist)] = Root(exp)

(* JunoIdleWake S3. WAKE_AT tells the truth: "covered" is sound only with this. *)
WakeAtTruthful ==
    \A c \in CPUS : pc[c] = "wfi" =>
        /\ (wakeAt[c] = TICKING => cval[c] <= now + 1)
        /\ (wakeAt[c] \notin {TICKING, ABSENT} => cval[c] <= wakeAt[c])

(* Section 5, point 1, derived here and not assumed: wherever a CPU can arm, and all the way
   to its own choose_wakeup, its wake slot says TICKING. The device counter of 1026/1027. *)
TickingWhenRunning == \A c \in CPUS : pc[c] # "wfi" => wakeAt[c] = TICKING

(* A CPU that is not in WFI has its tick within one tick (what lets Enter leave out
   STOPPED_AT_TICK). *)
TickSoon == \A c \in CPUS : pc[c] # "wfi" => cval[c] <= now + 1

(* Section 12, ExpiryTaken, as written there: while every CPU waits, every armed timer has a
   CPU that takes its interrupt by the deadline plus one tick. *)
ExpiryTaken ==
    (\A c \in CPUS : Waiting(c)) =>
        \A t \in TIMERS : Armed(t) => \E c \in CPUS : cval[c] <= Max(exp[t], now) + 1

(* The stronger form of JunoIdleWake S4 (ExpiryCovered): by the deadline itself, or by the
   next tick when the deadline is now or past. *)
ExpiryTakenTight ==
    (\A c \in CPUS : Waiting(c)) =>
        \A t \in TIMERS : Armed(t) => \E c \in CPUS : cval[c] <= Max(exp[t], now + 1)

(* Section 12, the bounded form: an armed timer is fired within EXP_BOUND ticks. *)
ExpiryInTime == \A t \in TIMERS : Armed(t) => now <= exp[t] + EXP_BOUND
=============================================================================
