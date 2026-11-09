----------------------------- MODULE Workspace -----------------------------
EXTENDS Naturals

Actors == {10, 11}
Groups == {20, 21}

VARIABLES place, actor, group, peer, choice, ownerKind, ownerId, payload

vars == <<place, actor, group, peer, choice, ownerKind, ownerId, payload>>

Init ==
  /\ place \in {"group", "private"}
  /\ actor \in Actors
  /\ group \in Groups
  /\ peer \in Actors
  /\ choice = [g \in Groups, a \in Actors |-> "group"]
  /\ ownerKind = "unset"
  /\ ownerId = 0
  /\ payload = 0

Personal ==
  /\ place = "group"
  /\ choice' = [choice EXCEPT ![group, actor] = "personal"]
  /\ ownerKind' = "unset"
  /\ ownerId' = 0
  /\ UNCHANGED <<place, actor, group, peer, payload>>

Restore ==
  /\ place = "group"
  /\ choice' = [choice EXCEPT ![group, actor] = "group"]
  /\ ownerKind' = "unset"
  /\ ownerId' = 0
  /\ UNCHANGED <<place, actor, group, peer, payload>>

SwitchActor(a) ==
  /\ a \in Actors
  /\ a # actor
  /\ actor' = a
  /\ ownerKind' = "unset"
  /\ ownerId' = 0
  /\ UNCHANGED <<place, group, peer, choice, payload>>

SwitchGroup(g) ==
  /\ place = "group"
  /\ g \in Groups
  /\ g # group
  /\ group' = g
  /\ ownerKind' = "unset"
  /\ ownerId' = 0
  /\ UNCHANGED <<place, actor, peer, choice, payload>>

SwitchPeer(p) ==
  /\ place = "private"
  /\ p \in Actors
  /\ p # peer
  /\ peer' = p
  /\ ownerKind' = "unset"
  /\ ownerId' = 0
  /\ UNCHANGED <<place, actor, group, choice, payload>>

SwitchPlace ==
  /\ place' = IF place = "group" THEN "private" ELSE "group"
  /\ ownerKind' = "unset"
  /\ ownerId' = 0
  /\ UNCHANGED <<actor, group, peer, choice, payload>>

Resolve ==
  /\ IF place = "private" /\ peer # actor
     THEN /\ ownerKind' = "refuse"
          /\ ownerId' = 0
     ELSE IF place = "private"
          THEN /\ ownerKind' = "personal"
               /\ ownerId' = actor
          ELSE IF choice[group, actor] = "personal"
               THEN /\ ownerKind' = "personal"
                    /\ ownerId' = actor
               ELSE /\ ownerKind' = "group"
                    /\ ownerId' = group
  /\ payload' = 0
  /\ UNCHANGED <<place, actor, group, peer, choice>>

Next ==
  \/ Personal
  \/ Restore
  \/ \E a \in Actors : SwitchActor(a)
  \/ \E g \in Groups : SwitchGroup(g)
  \/ \E p \in Actors : SwitchPeer(p)
  \/ SwitchPlace
  \/ Resolve

Spec == Init /\ [][Next]_vars

Inv ==
  /\ payload = 0
  /\ ownerKind \in {"unset", "personal", "group", "refuse"}
  /\ ownerKind = "personal" =>
       /\ ownerId = actor
       /\ place = "private" => peer = actor
       /\ place = "group" => choice[group, actor] = "personal"
  /\ ownerKind = "group" =>
       /\ ownerId = group
       /\ place = "group"
       /\ choice[group, actor] = "group"
  /\ ownerKind = "unset" => ownerId = 0
  /\ ownerKind = "refuse" => place = "private" /\ peer # actor /\ ownerId = 0
  /\ \A g \in Groups, a \in Actors : choice[g, a] \in {"group", "personal"}
=============================================================================
