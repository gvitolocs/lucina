---------------------------- MODULE JunoIdleWake ----------------------------
(***************************************************************************)
(* Juno's idle wake-up protocol, as the kernel does it (LucinaOS kernel     *)
(* patches up to 1163; the cpuidle driver of 1020-1023 is off by default    *)
(* and not modelled here).                                                   *)
(*                                                                           *)
(* Code modelled, in the applied Asterinas tree:                             *)
(*   ostd/src/task/preempt/mod.rs        halt_cpu                           *)
(*   ostd/src/arch/arm/irq/ops.rs        enable_local_and_halt (WFI)        *)
(*   ostd/src/arch/arm/timer/mod.rs      stop_tick_for_idle, choose_wakeup, *)
(*                                       wake_from_idle, resume_ticks,      *)
(*                                       timer_callback/set_next_timer,     *)
(*                                       WAKE_AT / WAKE_LOCK                *)
(*   ostd/libs/lucina-idle/src/lib.rs    covered, wake_at                   *)
(*   kernel/core/src/time/idle_tick.rs   next_event                         *)
(*   kernel/core/src/time/softirq.rs     timer softirq, global RUNNING skip *)
(*   ostd/src/task/scheduler/mod.rs      unpark_target, set_need_preempt,   *)
(*                                       might_preempt, the scheduler tick  *)
(*   ostd/src/smp.rs                     inter_processor_call, SGI handler  *)
(*   kernel/core/src/device/watchdog.rs  the per-CPU watchdog thread        *)
(*                                                                           *)
(* Time is the counter in ticks (STEP = 1 tick). It advances only when      *)
(* every CPU waits: in WFI or running user code with nothing pending. CPU   *)
(* steps are instantaneous next to a tick. A CPU's timer interrupt is       *)
(* pending while now >= its compare value (the virtual timer is a level     *)
(* PPI), SGIs and device interrupts are latched, and WFI ends on any of     *)
(* them with interrupts masked (ARM ARM D1.16.2).                           *)
(*                                                                           *)
(* The watchdog thread of CPU 0 is the thread we follow: real-time, pinned, *)
(* it sleeps PERIOD ticks on a timer of a system-wide timer manager.        *)
(*                                                                           *)
(* Mutant is "none" for the real protocol, or names a deliberate change     *)
(* that a witness configuration must see violate a property.                *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS CPUS,      \* CPU numbers; 0 is the BSP
          CAP,       \* MAX_IDLE_SLEEP (1 s) in ticks, scaled down
          PERIOD,    \* THREAD_PERIOD of the watchdog thread, in ticks
          TMAX,      \* how far time runs in the search
          DEVIRQS,   \* how many device interrupts may come in a run
          RUN_BOUND, \* a queued watchdog thread runs within this many ticks
          EXP_BOUND, \* an expired timer is processed within this many ticks
          Mutant,
          Regrid     \* the proposed fix: a compare value more than a tick ahead is put back on the
                     \* tick after now, as Linux's tick_nohz_restart does (hrtimer_forward(now))

ASSUME CAP >= 2 /\ PERIOD >= 1 /\ TMAX >= 1 /\ 0 \in CPUS

TICKING == 0              \* lucina_idle::TICKING
ABSENT  == TMAX + CAP + 100 \* lucina_idle::ABSENT: later than any time here
NONE    == -1

Min(a, b) == IF a < b THEN a ELSE b
Max(a, b) == IF a > b THEN a ELSE b

VARIABLES
    now,        \* the counter, in ticks
    pc,         \* per CPU: where it is
    ctx,        \* per CPU: "idle" (the idle thread) or "user" (a user thread) is current
    cval,       \* per CPU: COUNTER_VAL = CNTV_CVAL, the next timer interrupt
    stopped,    \* per CPU: TICK_STOPPED
    stoppedAt,  \* per CPU: STOPPED_AT_TICK
    twakes,     \* per CPU: IDLE_TIMER_WAKEUPS, saturated at 2
    wakeAt,     \* per CPU: WAKE_AT
    needP,      \* per CPU: need_preempt
    sgi,        \* per CPU: the IPI SGI pending at the GIC
    callQ,      \* per CPU: a set_need_preempt call queued in CALL_QUEUES
    softP,      \* per CPU: the timer softirq raised
    devP,       \* per CPU: a device interrupt pending
    userQ,      \* per CPU: a user thread queued on its run queue
    running,    \* the CPU in the timer softirq's callbacks (RUNNING), or NONE
    wState,     \* the watchdog thread: "waiting" (timer armed), "queued", "running"
    wExp,       \* its timer's expiry
    wQueuedAt,  \* when it was made runnable
    devLeft,    \* device interrupts left in this run
    jumped,     \* the counter-jump mutant has fired
    softSince   \* per CPU: when its timer softirq run began its later callbacks

vars == <<now, pc, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ,
          softP, devP, userQ, running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

TimerDue(c) == now >= cval[c]
Pending(c)  == TimerDue(c) \/ sgi[c] \/ devP[c]

\* Time may pass only while every CPU waits for an interrupt.
Waiting(c) == \/ pc[c] \in {"wfi", "user"} /\ ~Pending(c)
              \* The softirq's callbacks after the timer managers (coarse clock, RCU) may run across
              \* one tick boundary, with RUNNING held: the window of the RUNNING skip.
              \/ pc[c] = "softrest" /\ softSince[c] = now

Init ==
    /\ now = 0
    /\ pc = [c \in CPUS |-> "halt"]
    /\ ctx = [c \in CPUS |-> "idle"]
    /\ cval = [c \in CPUS |-> 1]
    /\ stopped = [c \in CPUS |-> FALSE]
    /\ stoppedAt = [c \in CPUS |-> 0]
    /\ twakes = [c \in CPUS |-> 2]
    /\ wakeAt = [c \in CPUS |-> TICKING]
    /\ needP = [c \in CPUS |-> FALSE]
    /\ sgi = [c \in CPUS |-> FALSE]
    /\ callQ = [c \in CPUS |-> FALSE]
    /\ softP = [c \in CPUS |-> FALSE]
    /\ devP = [c \in CPUS |-> FALSE]
    /\ userQ = [c \in CPUS |-> FALSE]
    /\ running = NONE
    /\ wState = "waiting"
    /\ wExp = PERIOD
    /\ wQueuedAt = 0
    /\ devLeft = DEVIRQS
    /\ jumped = FALSE
    /\ softSince = [c \in CPUS |-> 0]

-----------------------------------------------------------------------------
(* halt_cpu: interrupts off, then need_preempt is read. *)
HaltCheck(c) ==
    /\ pc[c] = "halt"
    \* A raised softirq left to softirqd: its thread is runnable, so the CPU runs it, not idle.
    /\ pc' = [pc EXCEPT ![c] = IF needP[c] \/ softP[c] THEN "irq" ELSE "stop"]
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ,
                   softP, devP, userQ, running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* next_event (idle_tick.rs): the earliest timer of the managers, 0 if already due. The
   load-average update is left out: it only adds wake-ups. *)
NextEvent(c) ==
    IF wState = "waiting"
    THEN IF Mutant = "expired_is_none" /\ wExp <= now THEN NONE ELSE Max(wExp - now, 0)
    ELSE NONE

(* stop_tick_for_idle + choose_wakeup + lucina_idle::{covered, wake_at}, all under WAKE_LOCK,
   so one step. STEP = 1, so idle_wakeup(now, nt, 1, d) is now + d when that is after nt. *)
StopTick(c) ==
    /\ pc[c] = "stop"
    /\ LET nt    == (IF Regrid /\ cval[c] > now + 1 THEN now + 1 ELSE cval[c])
           capW  == (IF now + CAP <= nt THEN NONE ELSE now + CAP)
           ev    == NextEvent(c)
           keep  == ev # NONE /\ ev < CAP /\ now + ev <= nt
           e     == (IF ev # NONE /\ ev < CAP THEN now + ev ELSE NONE)
           cov   == (IF Mutant = "always_covered" THEN TRUE
                     ELSE \E i \in CPUS \ {c} : wakeAt[i] # ABSENT /\ wakeAt[i] <= e)
           w     == (IF e # NONE /\ ~cov THEN Min(e, capW) ELSE capW)
           gate  == (IF Mutant = "no_gate" THEN TRUE ELSE twakes[c] >= 2)
       IN IF gate /\ capW # NONE /\ ~keep
          THEN /\ wakeAt' = [wakeAt EXCEPT ![c] = w]
               /\ stoppedAt' = [stoppedAt EXCEPT ![c] = nt]
               /\ stopped' = [stopped EXCEPT ![c] = TRUE]
               /\ cval' = [cval EXCEPT ![c] = w]
          ELSE /\ cval' = [cval EXCEPT ![c] = nt]
               /\ UNCHANGED <<wakeAt, stoppedAt, stopped>>
    /\ pc' = [pc EXCEPT ![c] = "wfi"]
    /\ UNCHANGED <<now, ctx, twakes, needP, sgi, callQ, softP, devP, userQ, running,
                   wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* WFI ends on any pending interrupt, masked or not. *)
WfiWake(c) ==
    /\ pc[c] = "wfi"
    /\ Pending(c)
    /\ pc' = [pc EXCEPT ![c] = "wake"]
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ,
                   softP, devP, userQ, running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* wake_from_idle, interrupts still off; resume_ticks with STEP = 1. *)
WakeFromIdle(c) ==
    /\ pc[c] = "wake"
    /\ LET byT == now >= cval[c] IN
       /\ twakes' = [twakes EXCEPT ![c] = IF byT THEN Min(twakes[c] + 1, 2) ELSE 0]
       /\ IF stopped[c]
          THEN /\ wakeAt' = [wakeAt EXCEPT ![c] =
                               IF Mutant = "stale_wake_at" THEN wakeAt[c] ELSE TICKING]
               /\ stopped' = [stopped EXCEPT ![c] = FALSE]
               /\ cval' = [cval EXCEPT ![c] =
                             IF byT THEN cval[c] ELSE Max(stoppedAt[c], now + 1)]
          ELSE /\ cval' = [cval EXCEPT ![c] =
                             IF Regrid /\ cval[c] > now + 1 THEN now + 1 ELSE cval[c]]
               /\ UNCHANGED <<wakeAt, stopped>>
    /\ pc' = [pc EXCEPT ![c] = "irq"]
    /\ UNCHANGED <<now, ctx, stoppedAt, needP, sgi, callQ, softP, devP, userQ, running,
                   wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

-----------------------------------------------------------------------------
(* Interrupts on: the GIC is claimed until nothing is pending (trap_handler's loop), then the
   bottom half (softirqs) runs. *)

(* The scheduler tick (update_current(Tick)): an idle current with a task queued, or a fair
   current with a real-time task queued, asks to be preempted. *)
SchedTickWants(c) ==
    \/ (c = 0 /\ wState = "queued")
    \/ (ctx[c] = "idle" /\ userQ[c])

(* timer_callback: callbacks (softirq raise, scheduler tick, cpufreq, watchdog check), then
   set_next_timer = COUNTER_VAL + STEP. *)
IrqTimer(c) ==
    /\ pc[c] = "irq"
    /\ TimerDue(c)
    /\ softP' = [softP EXCEPT ![c] = TRUE]
    /\ needP' = [needP EXCEPT ![c] = needP[c] \/ SchedTickWants(c)]
    /\ cval' = [cval EXCEPT ![c] = cval[c] + 1]
    /\ UNCHANGED <<now, pc, ctx, stopped, stoppedAt, twakes, wakeAt, sgi, callQ, devP, userQ,
                   running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* do_inter_processor_call: the queued calls set need_preempt. *)
IrqSgi(c) ==
    /\ pc[c] = "irq"
    /\ ~TimerDue(c)
    /\ sgi[c]
    /\ sgi' = [sgi EXCEPT ![c] = FALSE]
    /\ needP' = [needP EXCEPT ![c] = needP[c] \/ callQ[c]]
    /\ callQ' = [callQ EXCEPT ![c] = FALSE]
    /\ UNCHANGED <<now, pc, ctx, cval, stopped, stoppedAt, twakes, wakeAt, softP, devP, userQ,
                   running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* A device interrupt whose thread runs on this CPU (an IrqGate waiter, Wi-Fi, audio). *)
IrqDev(c) ==
    /\ pc[c] = "irq"
    /\ ~TimerDue(c) /\ ~sgi[c]
    /\ devP[c]
    /\ devP' = [devP EXCEPT ![c] = FALSE]
    /\ userQ' = [userQ EXCEPT ![c] = TRUE]
    /\ needP' = [needP EXCEPT ![c] = needP[c] \/ ctx[c] = "idle"]
    /\ UNCHANGED <<now, pc, ctx, cval, stopped, stoppedAt, twakes, wakeAt, sgi, callQ, softP,
                   running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* Nothing pending at the GIC: the timer softirq, if raised. process_all_pending runs it
   inline only while the CPU need not yield (Task::need_yield); otherwise it leaves it raised
   for softirqd, which runs once the preempting thread lets the CPU go (see HaltCheck). *)
IrqSoft(c) ==
    /\ pc[c] = "irq"
    /\ ~Pending(c)
    /\ softP[c]
    /\ ~needP[c]
    /\ softP' = [softP EXCEPT ![c] = FALSE]
    /\ IF running = NONE
       THEN /\ running' = c
            /\ pc' = [pc EXCEPT ![c] = "softrun"]
       ELSE /\ pc' = [pc EXCEPT ![c] = "irq"]   \* RUNNING held elsewhere: skip
            /\ UNCHANGED running
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, devP,
                   userQ, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* process_expired_timers: the watchdog's Waiter wakes, unpark_target enqueues it on CPU 0.
   The IPI is sent after the run queue lock is dropped: a separate step. *)
SoftRun(c) ==
    /\ pc[c] = "softrun"
    /\ IF wState = "waiting" /\ wExp <= now
       THEN /\ wState' = "queued"
            /\ wQueuedAt' = now
            /\ pc' = [pc EXCEPT ![c] = "softipi"]
       ELSE /\ pc' = [pc EXCEPT ![c] = "softrest"]
            /\ UNCHANGED <<wState, wQueuedAt>>
    /\ softSince' = [softSince EXCEPT ![c] = now]
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP,
                   devP, userQ, running, wExp, devLeft, jumped>>

(* The rest of the softirq's callbacks, RUNNING still held. *)
SoftRest(c) ==
    /\ pc[c] = "softrest"
    /\ pc' = [pc EXCEPT ![c] = "softrel"]
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP,
                   devP, userQ, running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* set_need_preempt(0): local flag on CPU 0, else a queued call and an SGI. *)
SoftIpi(c) ==
    /\ pc[c] = "softipi"
    /\ IF c = 0
       THEN /\ needP' = [needP EXCEPT ![0] = TRUE]
            /\ UNCHANGED <<callQ, sgi>>
       ELSE /\ callQ' = [callQ EXCEPT ![0] = TRUE]
            /\ sgi' = (IF Mutant = "no_ipi" THEN sgi ELSE [sgi EXCEPT ![0] = TRUE])
            /\ UNCHANGED needP
    /\ pc' = [pc EXCEPT ![c] = "softrel"]
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, softP, devP, userQ,
                   running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

SoftRel(c) ==
    /\ pc[c] = "softrel"
    /\ running' = NONE
    /\ pc' = [pc EXCEPT ![c] = "irq"]
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP,
                   devP, userQ, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* Back from the interrupt: might_preempt (or the return to the user thread). *)
IrqDone(c) ==
    /\ pc[c] = "irq"
    /\ ~Pending(c) /\ (~softP[c] \/ needP[c])
    /\ pc' = [pc EXCEPT ![c] = "sched"]
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP,
                   devP, userQ, running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* reschedule: need_preempt cleared, then the highest class runs: the RT watchdog thread on
   CPU 0, else a queued user thread, else the current one stays. *)
Sched(c) ==
    /\ pc[c] = "sched"
    /\ IF needP[c]
       THEN /\ needP' = [needP EXCEPT ![c] = FALSE]
            /\ IF c = 0 /\ wState = "queued"
               THEN /\ wState' = "running"
                    /\ pc' = [pc EXCEPT ![c] = "runW"]
                    /\ userQ' = [userQ EXCEPT ![c] = userQ[c] \/ ctx[c] = "user"]
                    /\ ctx' = [ctx EXCEPT ![c] = "idle"]  \* not user once preempted
               ELSE IF ctx[c] = "idle" /\ userQ[c]
                    THEN /\ userQ' = [userQ EXCEPT ![c] = FALSE]
                         /\ ctx' = [ctx EXCEPT ![c] = "user"]
                         /\ pc' = [pc EXCEPT ![c] = "user"]
                         /\ UNCHANGED wState
                    ELSE /\ pc' = [pc EXCEPT ![c] = IF ctx[c] = "user" THEN "user" ELSE "halt"]
                         /\ UNCHANGED <<wState, userQ, ctx>>
       ELSE /\ pc' = [pc EXCEPT ![c] = IF ctx[c] = "user" THEN "user" ELSE "halt"]
            /\ UNCHANGED <<needP, wState, userQ, ctx>>
    /\ UNCHANGED <<now, cval, stopped, stoppedAt, twakes, wakeAt, sgi, callQ, softP, devP,
                   running, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* The watchdog thread: notes the time, pets, sleeps PERIOD on a timer, parks. *)
RunW(c) ==
    /\ pc[c] = "runW"
    /\ wState' = "waiting"
    /\ wExp' = now + PERIOD
    /\ IF userQ[c]
       THEN /\ userQ' = [userQ EXCEPT ![c] = FALSE]
            /\ ctx' = [ctx EXCEPT ![c] = "user"]
            /\ pc' = [pc EXCEPT ![c] = "user"]
       ELSE /\ pc' = [pc EXCEPT ![c] = "halt"]
            /\ UNCHANGED <<userQ, ctx>>
    /\ UNCHANGED <<now, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP, devP,
                   running, wQueuedAt, devLeft, jumped, softSince>>

(* A user thread with interrupts on: a pending interrupt is taken. *)
UserIrq(c) ==
    /\ pc[c] = "user"
    /\ Pending(c)
    /\ pc' = [pc EXCEPT ![c] = "irq"]
    /\ UNCHANGED <<now, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP,
                   devP, userQ, running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* The user thread blocks: the CPU switches to its idle thread. *)
UserBlock(c) ==
    /\ pc[c] = "user"
    /\ ctx' = [ctx EXCEPT ![c] = "idle"]
    /\ pc' = [pc EXCEPT ![c] = "halt"]
    /\ UNCHANGED <<now, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP,
                   devP, userQ, running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* A device raises its line. *)
DevIrq(c) ==
    /\ devLeft > 0
    /\ ~devP[c]
    /\ devP' = [devP EXCEPT ![c] = TRUE]
    /\ devLeft' = devLeft - 1
    /\ UNCHANGED <<now, pc, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ,
                   softP, userQ, running, wState, wExp, wQueuedAt, jumped, softSince>>

(* Mutant "counter_jump": the virtual counter goes back (or COUNTER_VAL forward) on one CPU
   while it ticks, so that its next interrupt lies beyond the idle cap. This is a hardware or
   firmware assumption broken (CNTVOFF changed under the kernel), not a kernel step. *)
CounterJump(c) ==
    /\ Mutant \in {"counter_jump", "counter_jump_idle"}
    /\ ~jumped
    /\ ~stopped[c]
    /\ pc[c] = "halt"
    \* "counter_jump_idle": on a CPU about to idle (no preemption or softirq pending).
    /\ Mutant = "counter_jump_idle" => ~needP[c] /\ ~softP[c]
    /\ cval' = [cval EXCEPT ![c] = now + CAP + PERIOD + 2]
    /\ jumped' = TRUE
    /\ UNCHANGED <<now, pc, ctx, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP,
                   devP, userQ, running, wState, wExp, wQueuedAt, devLeft, softSince>>

Advance ==
    /\ now < TMAX
    /\ \A c \in CPUS : Waiting(c)
    /\ now' = now + 1
    /\ UNCHANGED <<pc, ctx, cval, stopped, stoppedAt, twakes, wakeAt, needP, sgi, callQ, softP,
                   devP, userQ, running, wState, wExp, wQueuedAt, devLeft, jumped, softSince>>

(* The search ends at TMAX; this is not a deadlock. *)
End ==
    /\ now = TMAX
    /\ \A c \in CPUS : Waiting(c)
    /\ UNCHANGED vars

CpuStep(c) ==
    \/ HaltCheck(c) \/ StopTick(c) \/ WfiWake(c) \/ WakeFromIdle(c)
    \/ IrqTimer(c) \/ IrqSgi(c) \/ IrqDev(c) \/ IrqSoft(c) \/ SoftRun(c) \/ SoftIpi(c)
    \/ SoftRest(c) \/ SoftRel(c) \/ IrqDone(c) \/ Sched(c) \/ RunW(c) \/ UserIrq(c) \/ UserBlock(c)
    \/ DevIrq(c) \/ CounterJump(c)

Next == Advance \/ End \/ \E c \in CPUS : CpuStep(c)

Spec == Init /\ [][Next]_vars

\* Liveness needs every CPU to keep taking its steps; time is not forced.
LiveSpec == Spec /\ \A c \in CPUS : WF_vars(CpuStep(c)) /\ WF_vars(Advance)

-----------------------------------------------------------------------------
(* Properties. *)

TypeOK ==
    /\ now \in 0..TMAX
    /\ \A c \in CPUS : pc[c] \in {"halt", "stop", "wfi", "wake", "irq", "softrun", "softipi",
                                   "softrel", "softrest", "sched", "runW", "user"}
    /\ running \in CPUS \cup {NONE}
    /\ wState \in {"waiting", "queued", "running"}

(* S1. A woken thread runs within RUN_BOUND ticks of its wake-up. *)
WokenRunsInTime == wState = "queued" => now <= wQueuedAt + RUN_BOUND

(* S2. An armed timer is processed (its thread woken) within EXP_BOUND ticks of its expiry. *)
ExpiryInTime == wState = "waiting" => now <= wExp + EXP_BOUND

(* S3. WAKE_AT tells the truth: a CPU in WFI is woken by its own timer no later than it
   published. "covered" is sound only with this. *)
WakeAtTruthful ==
    \A c \in CPUS : pc[c] = "wfi" =>
        /\ (wakeAt[c] = TICKING => cval[c] <= now + 1)
        /\ (wakeAt[c] \notin {TICKING, ABSENT} => cval[c] <= wakeAt[c])

(* S4. While everyone waits, some CPU will wake by the next expiry. *)
ExpiryCovered ==
    (wState = "waiting" /\ \A c \in CPUS : Waiting(c)) =>
        \E c \in CPUS : cval[c] <= Max(wExp, now + 1)

(* S5. No CPU idles in WFI with its own work pending past a tick: need_preempt or a queued
   call is never left with no interrupt to deliver it. *)
NoSilentWork ==
    \A c \in CPUS : pc[c] = "wfi" => ((needP[c] \/ callQ[c]) => (sgi[c] \/ TimerDue(c)))

Safety == TypeOK /\ WokenRunsInTime /\ ExpiryInTime /\ WakeAtTruthful /\ ExpiryCovered
          /\ NoSilentWork

(* L1. A queued watchdog thread runs. L2. An armed timer is processed. *)
QueuedRuns == (wState = "queued") ~> (wState = "running" \/ now = TMAX)
=============================================================================
