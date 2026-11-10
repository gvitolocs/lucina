------------------------------- MODULE JunoLock -------------------------------
(***************************************************************************)
(* juno-lock (buildhost ~/.local/bin/juno-lock): who may use the laptop.      *)
(* share: many at once; take: alone, after every sharer; a waiting take    *)
(* blocks new shares; takes go first come, first served.                   *)
(*                                                                           *)
(* The checks run inside a guard, `mkdir $G`. A process that fails to get  *)
(* it 100 times (10 s) removes it (`rmdir $G`), to get past a holder that  *)
(* died inside, and tries again. The guard's holder may stall (a stopped   *)
(* process, a slow file system, a starved CPU): the model lets it.         *)
(*                                                                           *)
(* Guard = "mkdir" is the script as it is; "flock" is the proposed fix: an *)
(* flock(1) on a lock file, which the kernel drops when its holder dies,   *)
(* so nobody ever needs to remove it.                                      *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS Procs,     \* each a session calling juno-lock with its own owner name
          Guard      \* "mkdir" or "flock"

VARIABLES
    pc,          \* per process
    want,        \* per process: "share" or "take"
    inGuard,     \* per process: it believes it holds the guard
    guardDir,    \* the guard directory exists ($G), or the flock is held (mkdir: any; flock: its holder)
    holder,      \* flock only: who holds it
    tries,       \* per process: failed guard attempts, saturated at the steal limit
    excl,        \* the process that holds $X, or "none"
    sharers,     \* the processes with a file in $S
    waiting      \* take waiters with a live marker in $P

vars == <<pc, want, inGuard, guardDir, holder, tries, excl, sharers, waiting>>

LIMIT == 2   \* "100 tries", scaled: a waiter at the limit removes the guard

Init ==
    /\ pc = [p \in Procs |-> "start"]
    /\ want = [p \in Procs |-> "share"]
    /\ inGuard = [p \in Procs |-> FALSE]
    /\ guardDir = FALSE
    /\ holder = "none"
    /\ tries = [p \in Procs |-> 0]
    /\ excl = "none"
    /\ sharers = {}
    /\ waiting = {}

Start(p) ==
    /\ pc[p] = "start"
    /\ \E m \in {"share", "take"} :
         /\ want' = [want EXCEPT ![p] = m]
         \* take with a wait > 0 writes its marker first (outside the guard)
         /\ waiting' = (IF m = "take" THEN waiting \cup {p} ELSE waiting)
    /\ pc' = [pc EXCEPT ![p] = "guard"]
    /\ UNCHANGED <<inGuard, guardDir, holder, tries, excl, sharers>>

(* guard(): until mkdir $G; do n++; [ n -gt 100 ] && rmdir $G; sleep; done *)
GuardTry(p) ==
    /\ pc[p] = "guard"
    /\ IF Guard = "flock"
       THEN /\ holder = "none"
            /\ holder' = p
            /\ inGuard' = [inGuard EXCEPT ![p] = TRUE]
            /\ pc' = [pc EXCEPT ![p] = "check"]
            /\ UNCHANGED <<guardDir, tries>>
       ELSE IF ~guardDir
            THEN /\ guardDir' = TRUE
                 /\ inGuard' = [inGuard EXCEPT ![p] = TRUE]
                 /\ tries' = [tries EXCEPT ![p] = 0]
                 /\ pc' = [pc EXCEPT ![p] = "check"]
                 /\ UNCHANGED holder
            ELSE /\ IF tries[p] >= LIMIT
                    THEN guardDir' = FALSE            \* rmdir $G: whoever holds it
                    ELSE UNCHANGED guardDir
                 /\ tries' = [tries EXCEPT ![p] = IF tries[p] < LIMIT THEN tries[p] + 1 ELSE LIMIT]
                 /\ UNCHANGED <<inGuard, pc, holder>>
    /\ UNCHANGED <<want, excl, sharers, waiting>>

(* Inside the guard, two steps each, as the script does them. share: `[ ! -d $X ] && [ -z "$(pending)" ]`,
   then (resources, then) `echo > $S/$owner`. take: `[ -z "$(ls -A $S)" ]` and the first waiter, then
   `mkdir $X`, which fails if $X exists. Only the guard makes each pair atomic. *)
Check(p) ==
    /\ pc[p] = "check"
    /\ IF want[p] = "share"
       THEN pc' = [pc EXCEPT ![p] = IF excl = "none" /\ waiting = {} THEN "write_share" ELSE "unguard_retry"]
       ELSE pc' = [pc EXCEPT ![p] = IF sharers = {} THEN "mkdir_x" ELSE "unguard_retry"]
    /\ UNCHANGED <<want, inGuard, guardDir, holder, tries, excl, sharers, waiting>>

WriteShare(p) ==
    /\ pc[p] = "write_share"
    /\ sharers' = sharers \cup {p}
    /\ pc' = [pc EXCEPT ![p] = "unguard_ok"]
    /\ UNCHANGED <<want, inGuard, guardDir, holder, tries, excl, waiting>>

MkdirX(p) ==
    /\ pc[p] = "mkdir_x"
    /\ IF excl = "none"
       THEN /\ excl' = p /\ waiting' = waiting \ {p} /\ pc' = [pc EXCEPT ![p] = "unguard_ok"]
       ELSE /\ pc' = [pc EXCEPT ![p] = "unguard_retry"] /\ UNCHANGED <<excl, waiting>>
    /\ UNCHANGED <<want, inGuard, guardDir, holder, tries, sharers>>

(* unguard(): rmdir $G, whoever made it. *)
Unguard(p) ==
    /\ pc[p] \in {"unguard_ok", "unguard_retry"}
    /\ IF Guard = "flock"
       THEN /\ holder' = "none" /\ UNCHANGED guardDir
       ELSE /\ guardDir' = FALSE /\ UNCHANGED holder
    /\ inGuard' = [inGuard EXCEPT ![p] = FALSE]
    /\ pc' = [pc EXCEPT ![p] = IF pc[p] = "unguard_ok" THEN "holds" ELSE "guard"]
    /\ UNCHANGED <<want, tries, excl, sharers, waiting>>

(* release: the same guard, then drop what it holds. Folded into one guarded step. *)
Release(p) ==
    /\ pc[p] = "holds"
    /\ pc' = [pc EXCEPT ![p] = "done"]
    /\ excl' = (IF excl = p THEN "none" ELSE excl)
    /\ sharers' = sharers \ {p}
    /\ UNCHANGED <<want, inGuard, guardDir, holder, tries, waiting>>

Next == \E p \in Procs : Start(p) \/ GuardTry(p) \/ Check(p) \/ WriteShare(p) \/ MkdirX(p) \/ Unguard(p) \/ Release(p)
Spec == Init /\ [][Next]_vars

(* A stalled guard holder is any holder that does not step for a while: the interleavings already let
   others run any number of steps between its Check and its Unguard. *)

-----------------------------------------------------------------------------
(* The laptop's rule: an exclusive holder is alone. *)
ExclusiveAlone == excl /= "none" => sharers = {}
(* Two processes never both believe they are inside the guard. *)
GuardMutex == \A p, q \in Procs : p /= q => ~(inGuard[p] /\ inGuard[q])
Safety == ExclusiveAlone /\ GuardMutex
=============================================================================
