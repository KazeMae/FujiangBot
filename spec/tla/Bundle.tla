------------------------------ MODULE Bundle ------------------------------
EXTENDS Naturals

VARIABLES phase, verified, sealed, worker, published, usable, executionAccepted,
          cleaned

vars == <<phase, verified, sealed, worker, published, usable, executionAccepted,
          cleaned>>

Init ==
  /\ phase = "pending"
  /\ verified = FALSE
  /\ sealed = FALSE
  /\ worker = "none"
  /\ published = FALSE
  /\ usable = FALSE
  /\ executionAccepted = FALSE
  /\ cleaned = FALSE

FreshVerify ==
  /\ phase = "pending"
  /\ verified = FALSE
  /\ worker = "none"
  /\ sealed = FALSE
  /\ published = FALSE
  /\ verified' = TRUE
  /\ phase' = "verified"
  /\ UNCHANGED <<sealed, worker, published, usable, executionAccepted, cleaned>>

Seal ==
  /\ phase = "verified"
  /\ verified = TRUE
  /\ sealed' = TRUE
  /\ phase' = "sealed"
  /\ UNCHANGED <<verified, worker, published, usable, executionAccepted, cleaned>>

Write ==
  /\ phase = "sealed"
  /\ verified = TRUE
  /\ sealed = TRUE
  /\ worker' = "writing"
  /\ phase' = "writing"
  /\ UNCHANGED <<verified, sealed, published, usable, executionAccepted, cleaned>>

ReapSuccess ==
  /\ phase = "writing"
  /\ verified = TRUE
  /\ sealed = TRUE
  /\ worker = "writing"
  /\ worker' = "reaped"
  /\ phase' = "reaped"
  /\ UNCHANGED <<verified, sealed, published, usable, executionAccepted, cleaned>>

Publish ==
  /\ phase = "reaped"
  /\ verified = TRUE
  /\ sealed = TRUE
  /\ worker = "reaped"
  /\ published = FALSE
  /\ published' = TRUE
  /\ usable' = TRUE
  /\ phase' = "published"
  /\ UNCHANGED <<verified, sealed, worker, executionAccepted, cleaned>>

PublishFail ==
  /\ phase = "reaped"
  /\ verified = TRUE
  /\ sealed = TRUE
  /\ worker = "reaped"
  /\ published = FALSE
  /\ cleaned' = TRUE
  /\ usable' = FALSE
  /\ phase' = "failed"
  /\ UNCHANGED <<verified, sealed, worker, published, executionAccepted>>

Kill ==
  /\ phase = "writing"
  /\ verified = TRUE
  /\ worker = "writing"
  /\ published = FALSE
  /\ worker' = "killed"
  /\ phase' = "killed"
  /\ UNCHANGED <<verified, sealed, published, usable, executionAccepted, cleaned>>

ReapFailure ==
  /\ phase = "killed"
  /\ verified = TRUE
  /\ worker = "killed"
  /\ published = FALSE
  /\ worker' = "reaped_error"
  /\ phase' = "reaped_error"
  /\ UNCHANGED <<verified, sealed, published, usable, executionAccepted, cleaned>>

Cleanup ==
  /\ phase = "reaped_error"
  /\ worker = "reaped_error"
  /\ published = FALSE
  /\ cleaned' = TRUE
  /\ usable' = FALSE
  /\ phase' = "failed"
  /\ UNCHANGED <<verified, sealed, worker, published, executionAccepted>>

FailEarly ==
  /\ phase \in {"pending", "verified", "sealed"}
  /\ worker = "none"
  /\ published = FALSE
  /\ cleaned' = TRUE
  /\ usable' = FALSE
  /\ phase' = "failed"
  /\ UNCHANGED <<verified, sealed, worker, published, executionAccepted>>

Done ==
  /\ phase \in {"published", "failed"}
  /\ UNCHANGED vars

Next ==
  \/ FreshVerify
  \/ Seal
  \/ Write
  \/ ReapSuccess
  \/ Publish
  \/ PublishFail
  \/ Kill
  \/ ReapFailure
  \/ Cleanup
  \/ FailEarly
  \/ Done

Spec == Init /\ [][Next]_vars

Inv ==
  /\ executionAccepted = FALSE
  /\ phase \in {"pending", "verified", "sealed", "writing", "reaped", "killed",
                "reaped_error", "published", "failed"}
  /\ worker \in {"none", "writing", "reaped", "killed", "reaped_error"}
  /\ phase = "pending" => verified = FALSE /\ sealed = FALSE /\ worker = "none"
  /\ phase \in {"verified", "sealed", "writing", "reaped", "killed", "reaped_error",
                "published"} => verified = TRUE
  /\ sealed = TRUE => phase # "pending" /\ phase # "verified"
  /\ worker = "writing" =>
       /\ phase = "writing"
       /\ verified = TRUE
       /\ sealed = TRUE
       /\ published = FALSE
       /\ usable = FALSE
  /\ worker = "reaped" =>
       /\ verified = TRUE
       /\ sealed = TRUE
       /\ phase \in {"reaped", "published", "failed"}
  /\ worker = "killed" =>
       /\ verified = TRUE
       /\ published = FALSE
       /\ usable = FALSE
       /\ phase = "killed"
  /\ worker = "reaped_error" =>
       /\ verified = TRUE
       /\ published = FALSE
       /\ usable = FALSE
       /\ phase \in {"reaped_error", "failed"}
  /\ published = TRUE =>
       /\ phase = "published"
       /\ verified = TRUE
       /\ sealed = TRUE
       /\ worker = "reaped"
       /\ usable = TRUE
  /\ usable = TRUE => published = TRUE /\ worker = "reaped" /\ sealed = TRUE /\ verified = TRUE
  /\ phase = "failed" =>
       /\ published = FALSE
       /\ usable = FALSE
       /\ cleaned = TRUE
       /\ worker \in {"none", "reaped", "reaped_error"}
  /\ phase = "published" => executionAccepted = FALSE
=============================================================================
