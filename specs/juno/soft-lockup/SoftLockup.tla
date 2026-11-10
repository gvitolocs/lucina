------------------------------ MODULE SoftLockup ------------------------------
(***************************************************************************)
(* The soft-lockup detector (kernel/core/src/device/watchdog.rs on_tick,  *)
(* cpu_thread; kernel/lucina-wdt Lockup::check). Each CPU's real-time     *)
(* thread notes the time every THREAD_PERIOD (4 s); every timer interrupt *)
(* on every CPU checks all CPUs' times and reports one away longer than   *)
(* the threshold (20 s). A check whose own CPU's previous tick is over    *)
(* clock_jump (5 s) old returns StartOver instead, and on_tick then sets  *)
(* every CPU's time to now. The hardware bark comes 25 s after CPU 0's    *)
(* thread last petted. Time is in seconds.                                 *)
(*                                                                           *)
(* Rule: "all" is the code; "own" resets only the checking CPU's entry;   *)
(* "global" wipes all only when no CPU ticked within clock_jump (time     *)
(* really did not run), and otherwise only the checking CPU's own entry.   *)
(* Scenario: "late" lets CPU 1's timer come up to LATE s late (a CPU deaf  *)
(* to its timer for a while, boot 138) while CPU 0's thread may stop for  *)
(* good; "suspend" lets the whole machine's time jump by JUMP s once,     *)
(* with no thread stuck (a paused VM, a long firmware call).               *)
(***************************************************************************)
EXTENDS Integers

CONSTANTS Rule, Scenario, THRESHOLD, CLOCK_JUMP, PERIOD, BARK, LATE, JUMP, TMAX

CPUS == {0, 1}
VARIABLES now, lastTick, touched, petted, stuck, jumped, reported

vars == <<now, lastTick, touched, petted, stuck, jumped, reported>>

Init ==
    /\ now = 1
    /\ lastTick = [c \in CPUS |-> 1]
    /\ touched = [c \in CPUS |-> 1]
    /\ petted = [c \in CPUS |-> 1]   \* ghost: when each thread really ran
    /\ stuck = FALSE
    /\ jumped = FALSE
    /\ reported = FALSE

MaxGap(c) == IF Scenario = "late" /\ c = 1 THEN LATE ELSE 1

(* cpu_thread: notes the time (and on CPU 0 pets the hardware). *)
Thread(c) ==
    /\ ~(c = 0 /\ stuck)
    /\ touched[c] /= now
    /\ touched' = [touched EXCEPT ![c] = now]
    /\ petted' = [petted EXCEPT ![c] = now]
    /\ UNCHANGED <<now, lastTick, stuck, jumped, reported>>

(* CPU 0's thread stops running for good (a CPU that stopped scheduling). *)
Stick ==
    /\ Scenario = "late"
    /\ ~stuck
    /\ stuck' = TRUE
    /\ UNCHANGED <<now, lastTick, touched, petted, jumped, reported>>

(* on_tick on CPU c: Lockup::check, then what on_tick does with the verdict. *)
Tick(c) ==
    /\ lastTick[c] /= now
    /\ LET gap == now - lastTick[c]
           others_ticked == \E d \in CPUS \ {c} : now - lastTick[d] <= CLOCK_JUMP
           startover == gap > CLOCK_JUMP
           wipe_all == startover /\ (Rule = "all" \/ (Rule = "global" /\ ~others_ticked))
           wipe_own == startover /\ ~wipe_all
           t2 == IF wipe_all THEN [d \in CPUS |-> now]
                 ELSE IF wipe_own THEN [touched EXCEPT ![c] = now] ELSE touched
       IN /\ touched' = t2
          /\ reported' = (reported \/ (~startover /\ \E d \in CPUS : now - touched[d] > THRESHOLD)
                                   \/ (wipe_own /\ Rule /= "all" /\ \E d \in CPUS \ {c} : now - touched[d] > THRESHOLD))
    /\ lastTick' = [lastTick EXCEPT ![c] = now]
    /\ UNCHANGED <<now, petted, stuck, jumped>>

(* Time passes only while each CPU has had its tick within its allowed gap and each running thread its period. *)
Advance ==
    /\ now < TMAX
    /\ \A c \in CPUS : now + 1 - lastTick[c] <= MaxGap(c)
    /\ \A c \in CPUS : (c = 0 /\ stuck) \/ now + 1 - touched[c] <= PERIOD \/ now + 1 - petted[c] <= PERIOD
    /\ now' = now + 1
    /\ UNCHANGED <<lastTick, touched, petted, stuck, jumped, reported>>

(* The whole machine's time jumps: no CPU ticked and no thread ran meanwhile. *)
Jump ==
    /\ Scenario = "suspend"
    /\ ~jumped
    /\ now + JUMP <= TMAX
    /\ now' = now + JUMP
    /\ jumped' = TRUE
    /\ UNCHANGED <<lastTick, touched, petted, stuck, reported>>

Done == now = TMAX /\ UNCHANGED vars

Next == Advance \/ Jump \/ Stick \/ Done \/ \E c \in CPUS : Thread(c) \/ Tick(c)
Spec == Init /\ [][Next]_vars

(* The detector reports a stuck CPU 0 before the hardware barks. *)
ReportBeforeBark == (stuck /\ now - petted[0] >= BARK) => reported
(* It never reports when no thread is stuck. *)
NoFalseReport == reported => stuck
=============================================================================
