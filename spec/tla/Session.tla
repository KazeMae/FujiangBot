------------------------------ MODULE Session ------------------------------
EXTENDS Naturals

VARIABLES
  persona, messages, summary, rev, phase, fresh, ticket,
  op, prevPersona, prevMessages, prevSummary, prevPhase, prevFresh, prevTicket, layered

vars == <<
  persona, messages, summary, rev, phase, fresh, ticket,
  op, prevPersona, prevMessages, prevSummary, prevPhase, prevFresh, prevTicket, layered
>>

Window == 2
MaxMsg == 4
MaxRev == 2
Personas == {"default", "strict"}
Phases == {"idle", "snapped", "ready"}
Tickets == {"none", "open", "checked", "used", "blocked"}

Init ==
  /\ persona \in Personas
  /\ messages \in 0..MaxMsg
  /\ summary \in BOOLEAN
  /\ rev \in 0..MaxRev
  /\ phase = "idle"
  /\ fresh = FALSE
  /\ ticket = "none"
  /\ op = "init"
  /\ prevPersona = persona
  /\ prevMessages = messages
  /\ prevSummary = summary
  /\ prevPhase = phase
  /\ prevFresh = fresh
  /\ prevTicket = ticket
  /\ layered = TRUE

Remember ==
  /\ prevPersona' = persona
  /\ prevMessages' = messages
  /\ prevSummary' = summary
  /\ prevPhase' = phase
  /\ prevFresh' = fresh
  /\ prevTicket' = ticket

Bump ==
  /\ IF rev < MaxRev THEN rev' = rev + 1 ELSE rev' = rev

Snap ==
  /\ phase = "idle"
  /\ messages > Window
  /\ phase' = "snapped"
  /\ fresh' = TRUE
  /\ ticket' = "open"
  /\ op' = "snap"
  /\ Remember
  /\ UNCHANGED <<persona, messages, summary, rev, layered>>

Validate ==
  /\ phase = "snapped"
  /\ fresh
  /\ ticket = "open"
  /\ phase' = "ready"
  /\ ticket' = "checked"
  /\ op' = "ready"
  /\ Remember
  /\ UNCHANGED <<persona, messages, summary, rev, fresh, layered>>

Commit ==
  /\ phase = "ready"
  /\ fresh
  /\ ticket = "checked"
  /\ messages' = Window
  /\ summary' = TRUE
  /\ phase' = "idle"
  /\ fresh' = FALSE
  /\ ticket' = "used"
  /\ op' = "commit"
  /\ Bump
  /\ Remember
  /\ UNCHANGED <<persona, layered>>

Fail ==
  /\ phase \in {"snapped", "ready"}
  /\ phase' = "idle"
  /\ fresh' = FALSE
  /\ ticket' = "none"
  /\ op' = "fail"
  /\ Remember
  /\ UNCHANGED <<persona, messages, summary, rev, layered>>

Dirty ==
  /\ fresh' = FALSE
  /\ ticket' = "blocked"
  /\ phase' = phase
  /\ Remember
  /\ UNCHANGED layered

Reset ==
  /\ messages' = 0
  /\ summary' = FALSE
  /\ persona' = persona
  /\ op' = "reset"
  /\ Bump
  /\ Dirty

Forget ==
  /\ summary' = FALSE
  /\ messages' = messages
  /\ persona' = persona
  /\ op' = "forget"
  /\ Bump
  /\ Dirty

Speak ==
  /\ messages < MaxMsg
  /\ messages' = messages + 1
  /\ summary' = summary
  /\ persona' = persona
  /\ op' = "speak"
  /\ Bump
  /\ Dirty

SetPersona ==
  /\ persona' = IF persona = "default" THEN "strict" ELSE "default"
  /\ messages' = messages
  /\ summary' = summary
  /\ op' = "persona"
  /\ Bump
  /\ Dirty

Next == Snap \/ Validate \/ Commit \/ Fail \/ Reset \/ Forget \/ Speak \/ SetPersona

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

TypeOk ==
  /\ persona \in Personas
  /\ messages \in 0..MaxMsg
  /\ summary \in BOOLEAN
  /\ rev \in 0..MaxRev
  /\ phase \in Phases
  /\ fresh \in BOOLEAN
  /\ ticket \in Tickets
  /\ prevPhase \in Phases
  /\ prevFresh \in BOOLEAN
  /\ prevTicket \in Tickets
  /\ layered = TRUE

TicketAgrees ==
  /\ ticket = "open" => phase = "snapped" /\ fresh
  /\ ticket = "checked" => phase = "ready" /\ fresh
  /\ ticket = "blocked" => ~fresh
  /\ phase = "idle" => ticket \in {"none", "used", "blocked"}

CommitOnlyAfterCheck ==
  op = "commit" =>
    /\ messages = Window
    /\ summary = TRUE
    /\ persona = prevPersona
    /\ prevMessages > Window
    /\ prevPhase = "ready"
    /\ prevFresh
    /\ prevTicket = "checked"
    /\ ticket = "used"

StaleCannotCommit ==
  /\ ticket = "blocked" => op /= "commit" /\ ticket /= "used"
  /\ (phase \in {"snapped", "ready"} /\ ~fresh) => op /= "commit" /\ ticket = "blocked"

FailKeeps ==
  op = "fail" =>
    /\ messages = prevMessages
    /\ summary = prevSummary
    /\ persona = prevPersona
    /\ prevPhase \in {"snapped", "ready"}

ResetClears ==
  op = "reset" => messages = 0 /\ summary = FALSE /\ persona = prevPersona /\ ~fresh

ForgetKeepsMessages ==
  op = "forget" =>
    /\ summary = FALSE
    /\ messages = prevMessages
    /\ persona = prevPersona
    /\ ~fresh

PersonaKeeps ==
  op = "persona" =>
    /\ persona /= prevPersona
    /\ messages = prevMessages
    /\ summary = prevSummary
    /\ ~fresh

Inv ==
  TypeOk /\ TicketAgrees /\ CommitOnlyAfterCheck /\ StaleCannotCommit
  /\ FailKeeps /\ ResetClears /\ ForgetKeepsMessages /\ PersonaKeeps

=============================================================================
