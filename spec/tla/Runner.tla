------------------------------ MODULE Runner ------------------------------
EXTENDS Naturals

VARIABLES authority, phase, pinned, payload, init, wrapper, reader, guard,
          spawned, result

vars == <<authority, phase, pinned, payload, init, wrapper, reader, guard,
          spawned, result>>

Init ==
  /\ authority \in {"accepted", "unaccepted"}
  /\ phase = "idle"
  /\ pinned = FALSE
  /\ payload = 0
  /\ init = "none"
  /\ wrapper = "none"
  /\ reader = "none"
  /\ guard = "none"
  /\ spawned = 0
  /\ result = "none"

Refuse ==
  /\ authority = "unaccepted"
  /\ phase = "idle"
  /\ result' = "fail"
  /\ phase' = "done"
  /\ UNCHANGED <<authority, pinned, payload, init, wrapper, reader, guard, spawned>>

StartBlock ==
  /\ authority = "accepted"
  /\ phase = "idle"
  /\ phase' = "blocked"
  /\ spawned' = 1
  /\ init' = "live"
  /\ wrapper' = "live"
  /\ reader' = "open"
  /\ guard' = "held"
  /\ UNCHANGED <<authority, pinned, payload, result>>

Pin ==
  /\ phase = "blocked"
  /\ pinned' = TRUE
  /\ phase' = "pinned"
  /\ UNCHANGED <<authority, payload, init, wrapper, reader, guard, spawned, result>>

Release ==
  /\ phase = "pinned"
  /\ pinned = TRUE
  /\ payload' = 1
  /\ phase' = "released"
  /\ UNCHANGED <<authority, pinned, init, wrapper, reader, guard, spawned, result>>

Reclaim ==
  /\ phase = "released"
  /\ init' = "dead"
  /\ wrapper' = "dead"
  /\ guard' = "disarmed"
  /\ phase' = "reclaimed"
  /\ UNCHANGED <<authority, pinned, payload, reader, spawned, result>>

Join ==
  /\ phase = "reclaimed"
  /\ guard = "disarmed"
  /\ init = "dead"
  /\ wrapper = "dead"
  /\ reader' = "reclaimed"
  /\ phase' = "joined"
  /\ UNCHANGED <<authority, pinned, payload, init, wrapper, guard, spawned, result>>

Succeed ==
  /\ phase = "joined"
  /\ result = "none"
  /\ pinned = TRUE
  /\ payload = 1
  /\ init = "dead"
  /\ wrapper = "dead"
  /\ reader = "reclaimed"
  /\ guard = "disarmed"
  /\ spawned = 1
  /\ result' = "success"
  /\ phase' = "done"
  /\ UNCHANGED <<authority, pinned, payload, init, wrapper, reader, guard, spawned>>

Fail ==
  /\ result = "none"
  /\ phase \in {"blocked", "pinned", "released", "reclaimed", "joined"}
  /\ result' = "fail"
  /\ phase' = "done"
  /\ UNCHANGED <<authority, pinned, payload, init, wrapper, reader, guard, spawned>>

Unknown ==
  /\ result = "none"
  /\ phase \in {"blocked", "pinned", "released"}
  /\ guard = "held"
  /\ init' = "unknown"
  /\ wrapper' = "unknown"
  /\ reader' = "unknown"
  /\ result' = "fail"
  /\ phase' = "done"
  /\ UNCHANGED <<authority, pinned, payload, guard, spawned>>

Done ==
  /\ phase = "done"
  /\ UNCHANGED vars

Next ==
  \/ Refuse
  \/ StartBlock
  \/ Pin
  \/ Release
  \/ Reclaim
  \/ Join
  \/ Succeed
  \/ Fail
  \/ Unknown
  \/ Done

Spec == Init /\ [][Next]_vars

Inv ==
  /\ authority \in {"accepted", "unaccepted"}
  /\ phase \in {"idle", "blocked", "pinned", "released", "reclaimed", "joined", "done"}
  /\ init \in {"none", "live", "dead", "unknown"}
  /\ wrapper \in {"none", "live", "dead", "unknown"}
  /\ reader \in {"none", "open", "reclaimed", "unknown"}
  /\ guard \in {"none", "held", "disarmed"}
  /\ result \in {"none", "success", "fail"}
  /\ payload \in {0, 1}
  /\ spawned \in {0, 1}
  /\ payload = 1 => pinned = TRUE
  /\ authority = "unaccepted" =>
       /\ spawned = 0
       /\ payload = 0
       /\ guard = "none"
       /\ init = "none"
       /\ wrapper = "none"
       /\ reader = "none"
  /\ guard = "disarmed" => init = "dead" /\ wrapper = "dead"
  /\ result = "success" =>
       /\ init = "dead"
       /\ wrapper = "dead"
       /\ reader = "reclaimed"
       /\ guard = "disarmed"
       /\ spawned = 1
       /\ payload = 1
       /\ pinned = TRUE
       /\ init # "unknown"
       /\ wrapper # "unknown"
       /\ reader # "unknown"
=============================================================================
