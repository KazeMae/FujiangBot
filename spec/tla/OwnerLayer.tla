----------------------------- MODULE OwnerLayer -----------------------------
EXTENDS Naturals

Owners == {"a", "b"}
BaseIno == 9

Other(o) == IF o = "a" THEN "b" ELSE "a"
InoOf(o) == IF o = "a" THEN 1 ELSE 2

VARIABLES policy, prod, lock, proc, meta, partial, ino, bytes, high, base, beat

vars == <<policy, prod, lock, proc, meta, partial, ino, bytes, high, base, beat>>

Init ==
  /\ policy \in BOOLEAN
  /\ prod = FALSE
  /\ lock = [o \in Owners |-> "free"]
  /\ proc = [o \in Owners |-> "out"]
  /\ meta = [o \in Owners |-> "none"]
  /\ partial = [o \in Owners |-> "no"]
  /\ ino = [o \in Owners |-> 0]
  /\ bytes = [o \in Owners |-> 0]
  /\ high = [o \in Owners |-> 0]
  /\ base = 1
  /\ beat = 0

Pulse ==
  /\ beat' = 1 - beat
  /\ UNCHANGED <<policy, prod, lock, proc, meta, partial, ino, bytes, high, base>>

Deny(o) ==
  /\ policy = FALSE
  /\ proc[o] = "out"
  /\ lock[o] = "free"
  /\ bytes[o] = 0
  /\ partial[o] = "no"
  /\ proc' = [proc EXCEPT ![o] = "denied"]
  /\ UNCHANGED <<policy, prod, lock, meta, partial, ino, bytes, high, base, beat>>

ClearDeny(o) ==
  /\ proc[o] = "denied"
  /\ proc' = [proc EXCEPT ![o] = "out"]
  /\ UNCHANGED <<policy, prod, lock, meta, partial, ino, bytes, high, base, beat>>

AbsLockStage(o) ==
  /\ policy = TRUE
  /\ lock[o] = "free"
  /\ proc[o] = "out"
  /\ meta[o] = "none"
  /\ bytes[o] = 0
  /\ partial[o] = "no"
  /\ lock' = [lock EXCEPT ![o] = "held"]
  /\ proc' = [proc EXCEPT ![o] = "staging"]
  /\ partial' = [partial EXCEPT ![o] = "yes"]
  /\ UNCHANGED <<policy, prod, meta, ino, bytes, high, base, beat>>

AbsPublish(o) ==
  /\ proc[o] = "staging"
  /\ lock[o] = "held"
  /\ partial[o] = "yes"
  /\ bytes[o] = 0
  /\ ino[o] = 0
  /\ proc' = [proc EXCEPT ![o] = "ready"]
  /\ partial' = [partial EXCEPT ![o] = "no"]
  /\ ino' = [ino EXCEPT ![o] = InoOf(o)]
  /\ bytes' = [bytes EXCEPT ![o] = base]
  /\ high' = [high EXCEPT ![o] = base]
  /\ meta' = [meta EXCEPT ![o] = "ok"]
  /\ UNCHANGED <<policy, prod, lock, base, beat>>

AbsCrashCleanup(o) ==
  /\ proc[o] = "staging"
  /\ lock[o] = "held"
  /\ partial[o] = "yes"
  /\ bytes[o] = 0
  /\ partial' = [partial EXCEPT ![o] = "no"]
  /\ proc' = [proc EXCEPT ![o] = "cleaned"]
  /\ UNCHANGED <<policy, prod, lock, meta, ino, bytes, high, base, beat>>

AbsUnlock(o) ==
  /\ lock[o] = "held"
  /\ proc[o] \in {"ready", "running", "cleaned"}
  /\ partial[o] = "no"
  /\ lock' = [lock EXCEPT ![o] = "free"]
  /\ proc' = [proc EXCEPT ![o] = "out"]
  /\ UNCHANGED <<policy, prod, meta, partial, ino, bytes, high, base, beat>>

AbsRun(o) ==
  /\ lock[o] = "held"
  /\ proc[o] = "ready"
  /\ meta[o] = "ok"
  /\ bytes[o] \in {1, 2}
  /\ proc' = [proc EXCEPT ![o] = "running"]
  /\ UNCHANGED <<policy, prod, lock, meta, partial, ino, bytes, high, base, beat>>

AbsMutate(o) ==
  /\ proc[o] = "running"
  /\ lock[o] = "held"
  /\ meta[o] = "ok"
  /\ bytes[o] = 1
  /\ high[o] = 1
  /\ bytes' = [bytes EXCEPT ![o] = 2]
  /\ high' = [high EXCEPT ![o] = 2]
  /\ UNCHANGED <<policy, prod, lock, proc, meta, partial, ino, base, beat>>

AbsReopen(o) ==
  /\ policy = TRUE
  /\ lock[o] = "free"
  /\ proc[o] = "out"
  /\ meta[o] = "ok"
  /\ bytes[o] \in {1, 2}
  /\ partial[o] = "no"
  /\ lock' = [lock EXCEPT ![o] = "held"]
  /\ proc' = [proc EXCEPT ![o] = "ready"]
  /\ UNCHANGED <<policy, prod, meta, partial, ino, bytes, high, base, beat>>

AbsTamper(o) ==
  /\ lock[o] = "free"
  /\ proc[o] = "out"
  /\ meta[o] = "ok"
  /\ bytes[o] \in {1, 2}
  /\ partial[o] = "no"
  /\ meta' = [meta EXCEPT ![o] = "bad"]
  /\ UNCHANGED <<policy, prod, lock, proc, partial, ino, bytes, high, base, beat>>

AbsReject(o) ==
  /\ policy = TRUE
  /\ lock[o] = "free"
  /\ proc[o] = "out"
  /\ meta[o] = "bad"
  /\ bytes[o] \in {1, 2}
  /\ proc' = [proc EXCEPT ![o] = "rejected"]
  /\ UNCHANGED <<policy, prod, lock, meta, partial, ino, bytes, high, base, beat>>

AbsAck(o) ==
  /\ proc[o] = "rejected"
  /\ proc' = [proc EXCEPT ![o] = "out"]
  /\ UNCHANGED <<policy, prod, lock, meta, partial, ino, bytes, high, base, beat>>

Next ==
  \/ Pulse
  \/ \E o \in Owners :
       \/ Deny(o)
       \/ ClearDeny(o)
       \/ AbsLockStage(o)
       \/ AbsPublish(o)
       \/ AbsCrashCleanup(o)
       \/ AbsUnlock(o)
       \/ AbsRun(o)
       \/ AbsMutate(o)
       \/ AbsReopen(o)
       \/ AbsTamper(o)
       \/ AbsReject(o)
       \/ AbsAck(o)

Spec == Init /\ [][Next]_vars

Inv ==
  /\ prod = FALSE
  /\ base = 1
  /\ \A o \in Owners :
       /\ lock[o] \in {"free", "held"}
       /\ proc[o] \in {"out", "denied", "staging", "ready", "running", "cleaned", "rejected"}
       /\ meta[o] \in {"none", "ok", "bad"}
       /\ partial[o] \in {"no", "yes"}
       /\ bytes[o] = high[o]
       /\ bytes[o] \in {0, 1, 2}
       /\ (proc[o] \in {"staging", "ready", "running", "cleaned"} => lock[o] = "held")
       /\ (proc[o] = "running" =>
            lock[o] = "held" /\ meta[o] = "ok" /\ bytes[o] \in {1, 2}
            /\ partial[o] = "no" /\ ino[o] = InoOf(o))
       /\ (proc[o] = "staging" =>
            partial[o] = "yes" /\ bytes[o] = 0 /\ ino[o] = 0 /\ meta[o] = "none")
       /\ (partial[o] = "yes" => proc[o] = "staging" /\ lock[o] = "held")
       /\ (proc[o] = "cleaned" =>
            partial[o] = "no" /\ bytes[o] = 0 /\ ino[o] = 0 /\ lock[o] = "held")
       /\ (meta[o] = "none" => bytes[o] = 0 /\ ino[o] = 0)
       /\ (meta[o] = "ok" =>
            bytes[o] \in {1, 2} /\ partial[o] = "no" /\ ino[o] = InoOf(o))
       /\ (meta[o] = "bad" =>
            bytes[o] \in {1, 2} /\ partial[o] = "no" /\ ino[o] = InoOf(o)
            /\ lock[o] = "free" /\ proc[o] \in {"out", "rejected"})
       /\ (proc[o] = "rejected" => meta[o] = "bad" /\ bytes[o] \in {1, 2})
       /\ (policy = FALSE =>
            bytes[o] = 0 /\ meta[o] = "none" /\ partial[o] = "no"
            /\ ino[o] = 0 /\ lock[o] = "free" /\ proc[o] \in {"out", "denied"})
       /\ (ino[o] # 0 =>
            ino[o] = InoOf(o) /\ ino[o] # BaseIno /\ ino[o] # ino[Other(o)]
            /\ bytes[o] \in {1, 2})
=============================================================================
