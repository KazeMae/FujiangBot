------------------------------ MODULE Session ------------------------------
EXTENDS Naturals

VARIABLES persona, messages, summary, op, prevPersona, prevMessages

vars == <<persona, messages, summary, op, prevPersona, prevMessages>>

Window == 1
Personas == {"default", "strict"}
Ops == {"init", "fold", "forget", "reset", "speak"}

Init ==
  /\ persona \in Personas
  /\ messages \in 0..3
  /\ summary \in BOOLEAN
  /\ op = "init"
  /\ prevPersona = persona
  /\ prevMessages = messages

Fold ==
  /\ messages > Window
  /\ persona' = persona
  /\ messages' = Window
  /\ summary' = TRUE
  /\ op' = "fold"
  /\ prevPersona' = persona
  /\ prevMessages' = messages

Forget ==
  /\ persona' = persona
  /\ messages' = messages
  /\ summary' = FALSE
  /\ op' = "forget"
  /\ prevPersona' = persona
  /\ prevMessages' = messages

Reset ==
  /\ persona' = persona
  /\ messages' = 0
  /\ summary' = FALSE
  /\ op' = "reset"
  /\ prevPersona' = persona
  /\ prevMessages' = messages

Speak ==
  /\ messages < 3
  /\ persona' = persona
  /\ messages' = messages + 1
  /\ summary' = summary
  /\ op' = "speak"
  /\ prevPersona' = persona
  /\ prevMessages' = messages

Next == Fold \/ Forget \/ Reset \/ Speak

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

TypeOk ==
  /\ persona \in Personas
  /\ messages \in 0..3
  /\ summary \in BOOLEAN
  /\ op \in Ops
  /\ prevPersona \in Personas
  /\ prevMessages \in 0..3

ResetClears ==
  op = "reset" => messages = 0 /\ summary = FALSE /\ persona = prevPersona

ForgetKeepsMessages ==
  op = "forget" => summary = FALSE /\ messages = prevMessages /\ persona = prevPersona

FoldKeepsPersona ==
  op = "fold" => persona = prevPersona /\ messages = Window /\ summary

Inv == TypeOk /\ ResetClears /\ ForgetKeepsMessages /\ FoldKeepsPersona

=============================================================================
