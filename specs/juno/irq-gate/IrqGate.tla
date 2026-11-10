------------------------------- MODULE IrqGate -------------------------------
(***************************************************************************)
(* ostd::sync::IrqGate (patch 1160) between a device's level interrupt and *)
(* the thread that waits for it, as Venus (1161), the GPU's fences (1162)   *)
(* and UFS (1163) use it.                                                   *)
(*                                                                           *)
(* Code: ostd/src/sync/irq_gate.rs (fire, wait, take_now), the verified     *)
(* steps in ostd/libs/lucina-irqgate (idle, fire, arm, woken, take), and    *)
(* the callers' loops (qcom_gpu.rs wait_fence, qcom_venus.rs events,        *)
(* lucina-ufs host.rs): check the device's own completion, and wait only    *)
(* when it is not done.                                                     *)
(*                                                                           *)
(* Each gate step runs under one spin lock with local interrupts off, so    *)
(* each is one step here; the device's enable is written inside it. The    *)
(* device latches its status bit; its line is (status AND enable), level    *)
(* high; the GIC delivers a level SPI while the line is up, and the handler *)
(* runs on some CPU. Verus proves each step's contract alone; this checks   *)
(* the steps against each other, the device and the waiters.               *)
(*                                                                           *)
(* Mutant: "none", or a deliberate change a witness configuration must     *)
(* catch.                                                                   *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS Waiters,   \* the threads that wait on this one gate (the contract: one)
          MAXJOBS,   \* how many jobs each waiter runs
          Mutant,
          WakeOthers \* the proposed fix for more than one waiter, two parts: (1) the wait's condition is
                     \* "something came, or my own completion is there" (the caller passes its check to
                     \* IrqGate::wait), so no one sleeps on a completion another waiter's take swallowed;
                     \* (2) every take, and an arm that takes bits, wakes the other sleepers (queue.wake_all),
                     \* because a take hands over bits that may be another's and disables the device that
                     \* another sleeper relies on

VARIABLES
    armed, pending,   \* the gate (lucina_irqgate::Gate); pending is a set of status bits
    devStatus,        \* the device's latched status bits (one bit per waiter's job here)
    devEn,            \* the device's interrupt enable
    busy,             \* jobs the device is working on
    pc,               \* per waiter
    jobs,             \* per waiter: jobs done
    handlerRuns       \* handler runs since a waiter last armed or took (for the storm bound)

vars == <<armed, pending, devStatus, devEn, busy, pc, jobs, handlerRuns>>

Bit(w) == {w}       \* the status bit of w's job
Line == devStatus /= {} /\ devEn

Init ==
    /\ armed = FALSE /\ pending = {}
    /\ devStatus = {} /\ devEn = FALSE
    /\ busy = {}
    /\ pc = [w \in Waiters |-> "submit"]
    /\ jobs = [w \in Waiters |-> 0]
    /\ handlerRuns = 0

-----------------------------------------------------------------------------
(* The device finishes a job: the completion is in memory (a fence word, a queue entry, a
   register the caller reads) and the status bit latches, enabled or not. *)
DeviceDone(w) ==
    /\ w \in busy
    /\ busy' = busy \ {w}
    /\ devStatus' = (IF Mutant = "no_latch" /\ ~devEn THEN devStatus ELSE devStatus \cup Bit(w))
    /\ UNCHANGED <<armed, pending, devEn, pc, jobs, handlerRuns>>

(* IrqGate::fire: under the lock, ack (read and clear the status, returning it), then
   lucina_irqgate::fire: a status disables the device's interrupt and wakes. *)
Handler ==
    /\ Line
    \* "level_stays": the mask is somewhere that does not drop the level (1131: the PDC alone), and the
    \* status is left for the thread: the line stays up. "no_disable": acked but not disabled.
    /\ LET status == devStatus IN
       /\ devStatus' = (IF Mutant = "level_stays" THEN devStatus ELSE {})
       /\ IF status /= {}
          THEN /\ pending' = pending \cup status
               /\ armed' = FALSE
               /\ devEn' = (IF Mutant \in {"no_disable", "level_stays"} THEN devEn ELSE FALSE)
          ELSE UNCHANGED <<pending, armed, devEn>>
    /\ handlerRuns' = handlerRuns + 1
    /\ UNCHANGED <<busy, pc, jobs>>

(* A waiter starts a job. *)
Submit(w) ==
    /\ pc[w] = "submit"
    /\ jobs[w] < MAXJOBS
    /\ busy' = busy \cup {w}
    /\ pc' = [pc EXCEPT ![w] = "check"]
    /\ UNCHANGED <<armed, pending, devStatus, devEn, jobs, handlerRuns>>

Done(w) == w \notin busy   \* its completion is in memory

(* The caller's loop: its own completion first. *)
Check(w) ==
    /\ pc[w] = "check"
    /\ IF Done(w)
       THEN /\ jobs' = [jobs EXCEPT ![w] = jobs[w] + 1]
            /\ pc' = [pc EXCEPT ![w] = "submit"]
       ELSE /\ pc' = [pc EXCEPT ![w] = "arm"]
            /\ UNCHANGED jobs
    /\ UNCHANGED <<armed, pending, devStatus, devEn, busy, handlerRuns>>

(* IrqGate::wait, its first locked step: lucina_irqgate::arm. *)
Arm(w) ==
    /\ pc[w] = "arm"
    /\ IF pending /= {}
       THEN /\ pending' = {}               \* taken without sleeping (Line::Keep)
            /\ armed' = FALSE
            /\ pc' = [v \in Waiters |-> IF v = w THEN "check"
                                         ELSE IF WakeOthers /\ pc[v] = "sleep" THEN "take" ELSE pc[v]]
            /\ UNCHANGED devEn
       ELSE /\ armed' = TRUE
            /\ devEn' = TRUE               \* Line::Enable
            /\ pc' = [pc EXCEPT ![w] = "sleep"]
            /\ UNCHANGED pending
    /\ handlerRuns' = 0
    /\ UNCHANGED <<devStatus, busy, jobs>>

(* The timed wait's condition, lucina_irqgate::woken, read under the lock. *)
Wake(w) ==
    /\ pc[w] = "sleep"
    /\ pending /= {} \/ (WakeOthers /\ Done(w))
    /\ pc' = [pc EXCEPT ![w] = "take"]
    /\ UNCHANGED <<armed, pending, devStatus, devEn, busy, jobs, handlerRuns>>

(* The timeout (Venus 50 ms, the GPU 1 s, UFS its request's): never fair. *)
Timeout(w) ==
    /\ pc[w] = "sleep"
    /\ pc' = [pc EXCEPT ![w] = "take"]
    /\ UNCHANGED <<armed, pending, devStatus, devEn, busy, jobs, handlerRuns>>

(* IrqGate::wait, its last locked step: lucina_irqgate::take. *)
Take(w) ==
    /\ pc[w] = "take"
    /\ devEn' = (IF armed THEN FALSE ELSE devEn)  \* Line::Disable when still armed
    /\ armed' = FALSE
    /\ pending' = {}
    /\ pc' = [v \in Waiters |-> IF v = w THEN "check"
                                 ELSE IF WakeOthers /\ pc[v] = "sleep" THEN "take" ELSE pc[v]]
    /\ handlerRuns' = 0
    /\ UNCHANGED <<devStatus, busy, jobs>>

Finished == \A w \in Waiters : pc[w] = "submit" /\ jobs[w] = MAXJOBS

Next ==
    \/ Handler
    \/ \E w \in Waiters : DeviceDone(w) \/ Submit(w) \/ Check(w) \/ Arm(w) \/ Wake(w) \/ Timeout(w) \/ Take(w)
    \/ (Finished /\ UNCHANGED vars)

Fair == /\ WF_vars(Handler)
        /\ \A w \in Waiters : WF_vars(DeviceDone(w)) /\ WF_vars(Submit(w)) /\ WF_vars(Check(w))
                              /\ WF_vars(Arm(w)) /\ WF_vars(Wake(w)) /\ WF_vars(Take(w))

Spec == Init /\ [][Next]_vars
LiveSpec == Spec /\ Fair

-----------------------------------------------------------------------------
(* Properties. *)

TypeOK == /\ armed \in BOOLEAN /\ pending \subseteq Waiters /\ devStatus \subseteq Waiters
          /\ \A w \in Waiters : pc[w] \in {"submit", "check", "arm", "sleep", "take"}

(* The Verus invariant, here across the device: armed exactly while the device may interrupt, and
   nothing pending while armed. *)
GateInv == /\ armed => devEn /\ pending = {}
           /\ (\E w \in Waiters : pc[w] = "sleep") /\ armed => devEn

(* No storm: a handler runs at most twice between two arms or takes (one with the status, one stray
   while the GIC still saw the level). The 1131 lesson: a mask that does not drop the level storms. *)
NoStorm == handlerRuns <= 2

(* No wake-up lost: a waiter asleep whose job is done is woken, not left to its timeout. *)
NoLostWakeup == \A w \in Waiters : (pc[w] = "sleep" /\ Done(w)) ~> pc[w] /= "sleep"

(* Every job finishes without relying on a timeout. *)
AllFinish == <>Finished
=============================================================================
