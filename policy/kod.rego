# Kod policy: the core. Default deny.
#
# The Rust API (crates/kod-policy) reads only data.kod.decision. A request
# is allowed when a grant rule for its action holds and no deny rule fires.
# Grant rules state every condition positively, so a missing or null field
# never allows anything. Deny rules explain refusals and can veto a grant.
# A data file <name>.json in this directory is data.config.<name>, so a
# data file can never shadow a rule.
package kod

decision := {"allow": allow, "reasons": reasons}

# An identifier from the caller (a login, repo, workspace, ticket, run or
# doc id) that is a string and not blank. Every rule that compares such an
# input with config or with another input checks it with this first, so ""
# never matches "".
named(s) if {
	is_string(s)
	trim_space(s) != ""
}

default allow := false

default grant := false

allow if {
	grant
	count(deny) == 0
}

reasons := sort(deny) if {
	count(deny) > 0
} else := [] if {
	allow
} else := [sprintf("%v: the request does not meet the rule for this action", [input.action])] if {
	true
}

# Every action the policy knows: one per row of the action and permission
# matrix.
known_actions := {
	"local_command",
	"model_request",
	"subscription_model_request",
	"read",
	"tracker_write",
	"draft_doc_write",
	"publish",
	"merge",
	"harness_change",
	"cancel",
	"resume",
	"sign_release",
	"delegate",
	"retention_delete",
}

known_action if input.action in known_actions

deny contains "unknown action" if not known_action

# Matrix rows enforced outside the policy engine. The policy never grants
# them, so no caller can treat an allow as authority for them.
external := {
	"local_command": "the sandbox",
	"sign_release": "trusted CI runners",
}

deny contains sprintf("%v is enforced by %v, not by policy", [input.action, by]) if {
	by := external[input.action]
}
