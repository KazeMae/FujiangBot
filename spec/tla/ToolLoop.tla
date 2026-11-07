------------------------------ MODULE ToolLoop ------------------------------
EXTENDS Naturals

VARIABLES phase, toolsEmpty, cap, used, modelCalls, overBudget

vars == <<phase, toolsEmpty, cap, used, modelCalls, overBudget>>

Init ==
  /\ phase = "running"
  /\ toolsEmpty \in BOOLEAN
  /\ cap \in 0..3
  /\ used = 0
  /\ modelCalls = 0
  /\ overBudget \in BOOLEAN

EmptyComplete ==
  /\ phase = "running"
  /\ ~overBudget
  /\ toolsEmpty
  /\ phase' = "complete"
  /\ modelCalls' = 1
  /\ UNCHANGED <<toolsEmpty, cap, used, overBudget>>

ModelText ==
  /\ phase = "running"
  /\ ~overBudget
  /\ ~toolsEmpty
  /\ phase' = "complete"
  /\ modelCalls' = modelCalls + 1
  /\ UNCHANGED <<toolsEmpty, cap, used, overBudget>>

ToolOk ==
  /\ phase = "running"
  /\ ~overBudget
  /\ ~toolsEmpty
  /\ used < cap
  /\ used' = used + 1
  /\ modelCalls' = modelCalls + 1
  /\ UNCHANGED <<phase, toolsEmpty, cap, overBudget>>

ToolErr ==
  /\ phase = "running"
  /\ ~overBudget
  /\ ~toolsEmpty
  /\ used < cap
  /\ phase' = "degraded"
  /\ modelCalls' = modelCalls + 1
  /\ UNCHANGED <<toolsEmpty, cap, used, overBudget>>

CapHit ==
  /\ phase = "running"
  /\ ~overBudget
  /\ ~toolsEmpty
  /\ used >= cap
  /\ phase' = "degraded"
  /\ modelCalls' = modelCalls + 1
  /\ UNCHANGED <<toolsEmpty, cap, used, overBudget>>

RejectBudget ==
  /\ phase = "running"
  /\ overBudget
  /\ phase' = "rejected"
  /\ UNCHANGED <<toolsEmpty, cap, used, modelCalls, overBudget>>

Next ==
  \/ EmptyComplete
  \/ ModelText
  \/ ToolOk
  \/ ToolErr
  \/ CapHit
  \/ RejectBudget

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

Inv ==
  /\ phase \in {"running", "complete", "degraded", "rejected"}
  /\ toolsEmpty \in BOOLEAN
  /\ cap \in 0..3
  /\ used \in 0..cap
  /\ modelCalls \in Nat
  /\ overBudget \in BOOLEAN
  /\ toolsEmpty => (used = 0 /\ phase /= "degraded" /\ modelCalls <= 1)
  /\ phase = "running" => modelCalls = used
  /\ phase \in {"complete", "degraded"} => modelCalls = used + 1 /\ ~overBudget
  /\ phase = "rejected" => overBudget /\ modelCalls = 0 /\ used = 0
  /\ phase = "degraded" => ~toolsEmpty

Terminates == <>(phase /= "running")

DegradedSticky == [](phase = "degraded" => [](phase = "degraded"))

CompleteSticky == [](phase = "complete" => [](phase = "complete"))

RejectedSticky == [](phase = "rejected" => [](phase = "rejected"))

=============================================================================
