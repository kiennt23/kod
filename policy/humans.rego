# Human and control operations: merge (requester_approval), harness changes,
# and cancel / kill switch. Callers: sub-plan 7 (requester-check and
# protected-paths) and sub-plan 6 (dispatcher).
package kod

# The spec's default for a team with no setting.
default_requester_approval := "not_sole"

# Policy::load refuses a team key that isn't lower-case and trimmed, and any
# setting other than allowed, not_sole or forbidden, so the lookup only
# canonicalises the caller's team.
# A merge without a named team is refused (see the deny rule below).
team_settings(team) := object.get(data.config.teams, lower(trim_space(team)), {}) if named(team)

requester_rule := object.get(team_settings(input.team), "requester_approval", default_requester_approval)

# Match the configuration gate: logins are non-empty and contain no whitespace.
valid_login(s) if {
	is_string(s)
	s != ""
	every c in split(s, "") {
		trim_space(c) == c
	}
}

human_approval(a) if {
	a.agent == false
	valid_login(a.login)
}

# GitHub logins are case-insensitive, so every login is compared lower-cased.
human_approvers := {lower(a.login) |
	is_array(input.approvals)
	some a in input.approvals
	human_approval(a)
}

requester_login := lower(input.requester) if valid_login(input.requester)

actor_login := lower(input.actor.login) if valid_login(input.actor.login)

requester_approved if requester_login in human_approvers

# The owner and, once named, the backup. Policy::load guarantees an owner
# login, a backup that is null or a login, and tech leads that are logins.
owners contains login if {
	some key in ["owner", "backup"]
	p := data.config.people[key]
	is_string(p)
	login := lower(p)
}

deny contains "approvals must be a list" if {
	input.action in {"merge", "harness_change"}
	input.approvals
	not is_array(input.approvals)
}

deny contains "merge needs a named team" if {
	input.action == "merge"
	not named(input.team)
}

deny contains "every approval must be from a named human; agents never approve" if {
	input.action in {"merge", "harness_change"}
	is_array(input.approvals)
	some a in input.approvals
	not human_approval(a)
}

# merge

grant if {
	input.action == "merge"
	requester_login
	requester_rule == "allowed"
	count(human_approvers) > 0
}

grant if {
	input.action == "merge"
	requester_login
	requester_rule == "not_sole"
	count(human_approvers - {requester_login}) > 0
}

grant if {
	input.action == "merge"
	requester_login
	requester_rule == "forbidden"
	count(human_approvers) > 0
	not requester_approved
}

deny contains "no human approval" if {
	input.action == "merge"
	count(human_approvers) == 0
}

deny contains "requester_approval is not_sole: the requester can't be the only approver" if {
	input.action == "merge"
	requester_rule == "not_sole"
	count(human_approvers) > 0
	count(human_approvers - {requester_login}) == 0
}

deny contains "requester_approval is forbidden: the requester approved" if {
	input.action == "merge"
	requester_rule == "forbidden"
	requester_approved
}

# harness_change: every named owner and backup approved.

approved_by(p) if p in human_approvers

grant if {
	input.action == "harness_change"
	every p in owners {
		approved_by(p)
	}
}

deny contains sprintf("harness change needs approval from %v", [p]) if {
	input.action == "harness_change"
	some p in owners
	not approved_by(p)
}

# cancel: one run by the owner, backup, a tech lead, or a label removal
# (only the run of the ticket that lost its label); every run (the kill
# switch) by the owner, backup or a tech lead. resume (restart after the
# kill switch) by the owner or backup only. When the policy fails to load or
# evaluate, the dispatcher still honours the owner's or backup's kill switch
# (identities configured in the dispatcher): a fail-closed policy must never
# block an emergency stop. A tech lead's stop goes through this policy.

cancelers contains p if {
	some p in owners
}

cancelers contains login if {
	some p in data.config.people.tech_leads
	login := lower(p)
}

grant if {
	input.action == "cancel"
	input.scope == "run"
	input.actor.kind == "label_removal"
	named(input.run.id)
	input.target.run == input.run.id
}

grant if {
	input.action == "cancel"
	input.scope == "run"
	input.actor.kind == "human"
	actor_login in cancelers
}

grant if {
	input.action == "cancel"
	input.scope == "all"
	input.actor.kind == "human"
	actor_login in cancelers
}

grant if {
	input.action == "resume"
	input.scope == "all"
	input.actor.kind == "human"
	actor_login in owners
}
