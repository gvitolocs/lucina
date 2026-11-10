----------------------------- MODULE CpufreqWriter -----------------------------
(***************************************************************************)
(* Patch 1182's cpufreq writer thread (kernel/core/src/device/qcom_cpufreq.rs). *)
(*                                                                           *)
(* on_tick (each ticking CPU's timer interrupt) stores the CPU's wanted     *)
(* clock and calls Domain::update, which sets the domain's dirty flag and   *)
(* wakes the writer (WRITER_WAKE.wake_one). The writer: wait_until(any      *)
(* domain dirty); then for each dirty domain, if its last change was under  *)
(* RATE_LIMIT_MS ago it notes the wait and skips it; otherwise it clears     *)
(* dirty and applies it (Domain::apply reads the wanted clocks, returns if   *)
(* the target is the current row, else writes). If it skipped any, it       *)
(* sleeps that long on a timer. WaitQueue::wait_until enqueues before it     *)
(* checks the condition, so a wake between the check and the sleep is not    *)
(* lost (ostd/src/sync/wait.rs).                                            *)
(*                                                                           *)
(* Time is in ms; a CPU ticks once per ms while it ticks. Wanted changes    *)
(* come from the load (nondeterministic, MAXCHANGES of them). Thread steps   *)
(* are instantaneous next to a tick.                                         *)
(*                                                                           *)
(* Variant "shipped" is 1182; "fixed" marks a domain dirty only when its     *)
(* wanted target differs from its current row, as update did before 1182.   *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS Domains, RATE, TMAX, MAXCHANGES, Variant

VARIABLES
    now,
    wanted,      \* per domain: the target its CPUs want (WANTED, reduced to the domain)
    current,     \* per domain: Domain::current, the row written last
    lastWrite,   \* per domain: last_write_ms
    dirty,       \* per domain
    wpc,         \* the writer: "wait" (on WRITER_WAKE), "run", "sleep" (rate-limit timer), "scan"
    wakeFlag,    \* a wake_one that reached the writer's waker
    sleepUntil,  \* the rate-limit timer's expiry
    soonest,     \* during a scan: the shortest rate-limit wait seen, or -1
    scanLeft,    \* during a scan: the domains still to look at
    changes,     \* wanted changes so far
    wakes,       \* writer wakes since the last real change of wanted
    spurious     \* writer wakes that found no clock differing from its row (saturated at MAXCHANGES + 1)

vars == <<now, wanted, current, lastWrite, dirty, wpc, wakeFlag, sleepUntil, soonest, scanLeft, changes, wakes, spurious>>

Init ==
    /\ now = 10
    /\ wanted = [d \in Domains |-> 0]
    /\ current = [d \in Domains |-> 0]
    /\ lastWrite = [d \in Domains |-> 0]
    /\ dirty = [d \in Domains |-> FALSE]
    /\ wpc = "wait"
    /\ wakeFlag = FALSE
    /\ sleepUntil = 0
    /\ soonest = -1
    /\ scanLeft = {}
    /\ changes = 0
    /\ wakes = 0
    /\ spurious = 0

(* The load changes what a domain's CPUs want (stored by their next tick). *)
LoadChange(d) ==
    /\ changes < MAXCHANGES
    /\ \E t \in {0, 1} : t /= wanted[d] /\ wanted' = [wanted EXCEPT ![d] = t]
    /\ changes' = changes + 1
    /\ UNCHANGED <<now, current, lastWrite, dirty, wpc, wakeFlag, sleepUntil, soonest, scanLeft, wakes, spurious>>

(* A tick of one of d's CPUs: on_tick then Domain::update. Ticks also come with nothing changed. *)
Tick(d) ==
    /\ LET mark == (IF Variant = "fixed" THEN wanted[d] /= current[d] ELSE TRUE) IN
       /\ dirty' = (IF mark THEN [dirty EXCEPT ![d] = TRUE] ELSE dirty)
       /\ wakeFlag' = (wakeFlag \/ (mark /\ wpc = "wait"))
    /\ UNCHANGED <<now, wanted, current, lastWrite, wpc, sleepUntil, soonest, scanLeft, changes, wakes, spurious>>

(* WaitQueue::wait_until: the condition holds, or a wake came. *)
WriterWake ==
    /\ wpc = "wait"
    /\ (\E d \in Domains : dirty[d]) \/ wakeFlag
    /\ IF \E d \in Domains : dirty[d]
       THEN /\ wpc' = "scan" /\ scanLeft' = Domains /\ soonest' = -1
       ELSE /\ UNCHANGED <<wpc, scanLeft, soonest>>   \* woken, condition false: waits again
    /\ wakeFlag' = FALSE
    /\ wakes' = (IF wakes <= MAXCHANGES + 1 THEN wakes + 1 ELSE wakes)
    /\ spurious' = (IF (\A d \in Domains : wanted[d] = current[d]) /\ spurious <= MAXCHANGES THEN spurious + 1 ELSE spurious)
    /\ UNCHANGED <<now, wanted, current, lastWrite, dirty, sleepUntil, changes>>

(* One domain of the scan. *)
Scan(d) ==
    /\ wpc = "scan"
    /\ d \in scanLeft
    /\ scanLeft' = scanLeft \ {d}
    /\ IF ~dirty[d]
       THEN UNCHANGED <<dirty, current, lastWrite, soonest>>
       ELSE IF now < lastWrite[d] + RATE
            THEN /\ soonest' = (IF soonest = -1 \/ lastWrite[d] + RATE - now < soonest
                                THEN lastWrite[d] + RATE - now ELSE soonest)
                 /\ UNCHANGED <<dirty, current, lastWrite>>
            ELSE /\ dirty' = [dirty EXCEPT ![d] = FALSE]
                 /\ IF wanted[d] /= current[d]
                    THEN /\ current' = [current EXCEPT ![d] = wanted[d]]
                         /\ lastWrite' = [lastWrite EXCEPT ![d] = now]
                    ELSE UNCHANGED <<current, lastWrite>>
                 /\ UNCHANGED soonest
    /\ UNCHANGED <<now, wanted, wpc, wakeFlag, sleepUntil, changes, wakes, spurious>>

ScanDone ==
    /\ wpc = "scan"
    /\ scanLeft = {}
    /\ IF soonest /= -1
       THEN /\ wpc' = "sleep" /\ sleepUntil' = now + soonest
       ELSE /\ wpc' = "wait" /\ UNCHANGED sleepUntil
    /\ UNCHANGED <<now, wanted, current, lastWrite, dirty, wakeFlag, soonest, scanLeft, changes, wakes, spurious>>

SleepEnd ==
    /\ wpc = "sleep"
    /\ now >= sleepUntil
    /\ wpc' = "wait"
    /\ UNCHANGED <<now, wanted, current, lastWrite, dirty, wakeFlag, sleepUntil, soonest, scanLeft, changes, wakes, spurious>>

(* Time passes when the writer is waiting or asleep with nothing due; every domain ticks at least once a ms
   while its wanted clock differs (its CPUs are busy then). *)
Advance ==
    /\ now < TMAX
    /\ wpc \in {"wait", "sleep"}
    /\ ~(wpc = "sleep" /\ now >= sleepUntil)
    /\ ~(wpc = "wait" /\ ((\E d \in Domains : dirty[d]) \/ wakeFlag))
    /\ now' = now + 1
    /\ UNCHANGED <<wanted, current, lastWrite, dirty, wpc, wakeFlag, sleepUntil, soonest, scanLeft, changes, wakes, spurious>>

Done == now = TMAX /\ UNCHANGED vars

Next == Advance \/ Done \/ WriterWake \/ ScanDone \/ SleepEnd
        \/ \E d \in Domains : LoadChange(d) \/ Tick(d) \/ Scan(d)

Spec == Init /\ [][Next]_vars
Fair == WF_vars(WriterWake) /\ WF_vars(ScanDone) /\ WF_vars(SleepEnd) /\ WF_vars(Advance)
        /\ \A d \in Domains : WF_vars(Tick(d)) /\ WF_vars(Scan(d))
LiveSpec == Spec /\ Fair

(* Every change reaches the hardware. *)
Applied == \A d \in Domains : (wanted[d] /= current[d]) ~> (wanted[d] = current[d] \/ now = TMAX)
(* The writer wakes with nothing to change at most once per change of a wanted clock (a clock can change and change
   back before the writer runs). *)
NoSpuriousWake == spurious <= changes
=============================================================================
