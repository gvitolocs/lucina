------------------------------ MODULE CpufreqL3 ------------------------------
(***************************************************************************)
(* The SC7180's cpufreq writers: each cluster's perf-state row and the     *)
(* L3's, which both clusters share (kernel/core/src/device/qcom_cpufreq.rs *)
(* at 1117: Domain::update, Domain::write, Driver::update_l3, on_tick).    *)
(*                                                                           *)
(* Ours: every CPU's timer tick runs on_tick in interrupt context. A domain *)
(* is updated under its own try_lock (a CPU that finds it taken skips), and *)
(* after the domain's row it updates the L3 with no lock across domains:   *)
(* it reads every domain's current step, computes the L3 row, swaps it     *)
(* into l3.current, and writes the register only if the swap returned a    *)
(* different row.                                                         *)
(*                                                                           *)
(* Linux (qcom-cpufreq-hw.c, osm-l3.c, Linux 7.1.10): one writer per       *)
(* policy (schedutil's kthread, target_index), and the L3 vote goes through *)
(* icc_set_bw to qcom_osm_l3_set under the interconnect mutex, aggregated  *)
(* over all clusters before one write. Mode = "linux" models that: the L3  *)
(* step (read, aggregate, write) is one critical section.                  *)
(*                                                                           *)
(* Steps are 0 (low) and 1 (high); the L3 row a domain needs equals its    *)
(* step (the OPP table's L3 column rises with the CPU step).               *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS Domains, MAXUPD, Mode

VARIABLES
    dstep,     \* per domain: Domain::current (the row written last)
    hwL3,      \* the L3 register's row (what the hardware was told)
    l3cur,     \* L3::current (what the software believes it wrote)
    pc,        \* per domain: its updater (the CPU holding the domain's try_lock)
    target,    \* per domain: the row being written
    seen,      \* per domain: the L3 row it computed from the domains' steps
    old,       \* per domain: what its swap returned
    l3lock,    \* "linux" only: the interconnect mutex (the domain holding it, or -1)
    updates    \* updates so far

vars == <<dstep, hwL3, l3cur, pc, target, seen, old, l3lock, updates>>

Max(S) == CHOOSE m \in S : \A x \in S : x <= m
Need == Max({dstep[d] : d \in Domains})

Init ==
    /\ dstep = [d \in Domains |-> 0]
    /\ hwL3 = 0 /\ l3cur = 0
    /\ pc = [d \in Domains |-> "idle"]
    /\ target = [d \in Domains |-> 0]
    /\ seen = [d \in Domains |-> 0]
    /\ old = [d \in Domains |-> 0]
    /\ l3lock = -1
    /\ updates = 0

(* on_tick on one of d's CPUs: wanted differs from current, and it got the domain's try_lock. *)
Begin(d) ==
    /\ pc[d] = "idle"
    /\ updates < MAXUPD
    /\ \E t \in {0, 1} :
         /\ t /= dstep[d]
         /\ target' = [target EXCEPT ![d] = t]
    /\ pc' = [pc EXCEPT ![d] = "write"]
    /\ updates' = updates + 1
    /\ UNCHANGED <<dstep, hwL3, l3cur, seen, old, l3lock>>

(* Domain::write: the perf-state row, then current. *)
WriteDomain(d) ==
    /\ pc[d] = "write"
    /\ dstep' = [dstep EXCEPT ![d] = target[d]]
    /\ pc' = [pc EXCEPT ![d] = IF Mode = "linux" THEN "lock" ELSE "read"]
    /\ UNCHANGED <<hwL3, l3cur, target, seen, old, l3lock, updates>>

(* linux: icc_set_bw takes the interconnect mutex. *)
Lock(d) ==
    /\ pc[d] = "lock"
    /\ l3lock = -1
    /\ l3lock' = d
    /\ pc' = [pc EXCEPT ![d] = "read"]
    /\ UNCHANGED <<dstep, hwL3, l3cur, target, seen, old, updates>>

(* update_l3: every domain's current step, aggregated. *)
Read(d) ==
    /\ pc[d] = "read"
    /\ seen' = [seen EXCEPT ![d] = Need]
    /\ pc' = [pc EXCEPT ![d] = "swap"]
    /\ UNCHANGED <<dstep, hwL3, l3cur, target, old, l3lock, updates>>

(* l3.current.swap(position). *)
Swap(d) ==
    /\ pc[d] = "swap"
    /\ old' = [old EXCEPT ![d] = l3cur]
    /\ l3cur' = seen[d]
    /\ pc' = [pc EXCEPT ![d] = "hw"]
    /\ UNCHANGED <<dstep, hwL3, target, seen, l3lock, updates>>

(* The register, only if the swap changed the row. *)
WriteL3(d) ==
    /\ pc[d] = "hw"
    /\ hwL3' = (IF old[d] /= seen[d] THEN seen[d] ELSE hwL3)
    /\ l3lock' = (IF Mode = "linux" THEN -1 ELSE l3lock)
    /\ pc' = [pc EXCEPT ![d] = "idle"]
    /\ UNCHANGED <<dstep, l3cur, target, seen, old, updates>>

Next == \/ \E d \in Domains : Begin(d) \/ WriteDomain(d) \/ Lock(d) \/ Read(d) \/ Swap(d) \/ WriteL3(d)
        \/ (updates = MAXUPD /\ (\A d \in Domains : pc[d] = "idle") /\ UNCHANGED vars)

Spec == Init /\ [][Next]_vars

Quiet == \A d \in Domains : pc[d] = "idle"

(* Between updates, the L3 register holds what the software believes it holds. *)
L3Consistent == Quiet => hwL3 = l3cur
(* Between updates, the L3 is voted at least as high as the clusters' steps need. *)
L3Covers == Quiet => hwL3 >= Need
(* And no higher: a stale high vote is power, not a hazard; reported, not required. *)
L3Exact == Quiet => hwL3 = Need
=============================================================================
