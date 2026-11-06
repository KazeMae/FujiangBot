------------------------------ MODULE ToolLoop ------------------------------
EXTENDS Naturals

VARIABLES phase, toolsEmpty, cap, used, modelCalls

vars == <<phase, toolsEmpty, cap, used, modelCalls>>

Init ==
  /\ phase = "running"
  /\ toolsEmpty \in BOOLEAN
  /\ cap \in 0..3
  /\ used = 0
  /\ modelCalls = 0

EmptyComplete ==
  /\ phase = "running"
  /\ toolsEmpty
  /\ phase' = "complete"
  /\ modelCalls' = 1
  /\ UNCHANGED <<toolsEmpty, cap, used>>

ModelText ==
  /\ phase = "running"
  /\ ~toolsEmpty
  /\ phase' = "complete"
  /\ modelCalls' = modelCalls + 1
  /\ UNCHANGED <<toolsEmpty, cap, used>>

ToolOk ==
  /\ phase = "running"
  /\ ~toolsEmpty
  /\ used < cap
  /\ used' = used + 1
  /\ modelCalls' = modelCalls + 1
  /\ UNCHANGED <<phase, toolsEmpty, cap>>

ToolErr ==
  /\ phase = "running"
  /\ ~toolsEmpty
  /\ used < cap
  /\ phase' = "degraded"
  /\ modelCalls' = modelCalls + 1
  /\ UNCHANGED <<toolsEmpty, cap, used>>

CapHit ==
  /\ phase = "running"
  /\ ~toolsEmpty
  /\ used >= cap
  /\ phase' = "degraded"
  /\ modelCalls' = modelCalls + 1
  /\ UNCHANGED <<toolsEmpty, cap, used>>

Next ==
  \/ EmptyComplete
  \/ ModelText
  \/ ToolOk
  \/ ToolErr
  \/ CapHit

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

Inv ==
  /\ phase \in {"running", "complete", "degraded"}
  /\ toolsEmpty \in BOOLEAN
  /\ cap \in 0..3
  /\ used \in 0..cap
  /\ modelCalls \in Nat
  /\ toolsEmpty => (used = 0 /\ phase /= "degraded" /\ modelCalls <= 1)
  /\ phase = "running" => modelCalls = used
  /\ phase /= "running" => modelCalls = used + 1
  /\ phase = "degraded" => ~toolsEmpty

Terminates == <>(phase /= "running")

DegradedSticky == [](phase = "degraded" => [](phase = "degraded"))

CompleteSticky == [](phase = "complete" => [](phase = "complete"))

=============================================================================
