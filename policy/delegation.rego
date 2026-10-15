# Delegation policy (spec, "Configurable policies"): which tickets the agent
# may take. Every ticket carries a risk class; path rules can only raise it.
# Caller: sub-plan 6 (dispatcher), which also makes admission atomic.
package kod

risk := data.config.delegation.risk_classes

ticket_paths := object.get(object.get(input, "ticket", {}), "paths", [])

# The defaults hold for every repo; a repo's own rules (set in config, per
# repo) add to them. Rules only raise the class, so adding is always safe.
# repo_path_rules keys are lower-case owner/name (Policy::load refuses any
# other key, and any rule that isn't exactly {prefix, class} with a known
# class). GitHub names are case-insensitive, so the lookup lower-cases and
# trims the repo.
repo_rules(repo) := object.get(data.config.delegation.repo_path_rules, lower(trim_space(repo)), []) if named(repo)

repo_rules(repo) := [] if not named(repo)

rules_for(repo) := array.concat(data.config.delegation.default_path_rules, repo_rules(repo))

ticket_repo := object.get(object.get(input, "ticket", {}), "repo", null)

run_repo := object.get(object.get(input, "run", {}), "repo", null)

skip_segment(seg) if seg in {"", "."}

# Policy::load refuses any path-rule prefix that isn't already in this form.
# Normalised for matching: trimmed, lower-cased, backslashes read as
# slashes, empty and "." segments dropped. Only for strings.
path_segments(p) := [seg |
	some seg in split(replace(lower(trim_space(p)), "\\", "/"), "/")
	not skip_segment(seg)
] if is_string(p)

clean_path(p) := concat("/", path_segments(p))

# Refused outright: not a string, empty, or with a ".." segment.
bad_path(p) if not is_string(p)

bad_path(p) if clean_path(p) == ""

bad_path(p) if ".." in path_segments(p)

# A rule matches at the top of the repo or below any directory, so
# src/auth/x is raised like auth/x.
# The path gets a trailing / so a path equal to a directory rule (auth, src/auth)
# matches its auth/ prefix; authx still does not.
path_matches(cp, prefix) if startswith(concat("", [cp, "/"]), prefix)

path_matches(cp, prefix) if contains(concat("", ["/", cp, "/"]), concat("", ["/", prefix]))

bad_paths_in(paths) := {p | some p in paths; bad_path(p)}

ranks_in(paths, rules) := {risk[r.class] |
	some p in paths
	some r in rules
	path_matches(clean_path(p), r.prefix)
}

class_allowed(c) if c in data.config.delegation.allowed_classes

bad_paths := bad_paths_in(ticket_paths) if is_array(ticket_paths)

raised_ranks := ranks_in(ticket_paths, rules_for(ticket_repo)) if is_array(ticket_paths)

effective_rank := max(raised_ranks | {risk[input.ticket.risk_class]})

effective_class := c if {
	some c, rank in risk
	rank == effective_rank
}

class_delegated if class_allowed(effective_class)

has_criteria if input.ticket.acceptance_criteria == true

grant if {
	input.action == "delegate"
	named(ticket_repo)
	is_array(ticket_paths)
	count(bad_paths) == 0
	class_delegated
	has_criteria
	is_number(input.wip_in_progress)
	input.wip_in_progress >= 0
	input.wip_in_progress < data.config.delegation.wip_limit
	is_number(input.ticket.bounces)
	input.ticket.bounces >= 0
	input.ticket.bounces < data.config.delegation.bounce_limit
}

deny contains "ticket has no repo" if {
	input.action == "delegate"
	not named(ticket_repo)
}

deny contains "ticket has no risk class" if {
	input.action == "delegate"
	not input.ticket.risk_class
}

deny contains sprintf("unknown risk class: %v", [input.ticket.risk_class]) if {
	input.action == "delegate"
	input.ticket.risk_class
	not risk[input.ticket.risk_class]
}

deny contains sprintf("risk class %v is not delegated in this phase", [effective_class]) if {
	input.action == "delegate"
	effective_class
	not class_delegated
}

deny contains sprintf("path %v raises the risk class to %v", [p, r.class]) if {
	input.action == "delegate"
	not class_delegated
	is_array(ticket_paths)
	some p in ticket_paths
	some r in rules_for(ticket_repo)
	path_matches(clean_path(p), r.prefix)
	risk[r.class] > risk[input.ticket.risk_class]
}

deny contains "ticket.paths must be a list" if {
	input.action == "delegate"
	not is_array(ticket_paths)
}

deny contains sprintf("path %v is not a clean repo-relative path", [p]) if {
	input.action == "delegate"
	some p in bad_paths
}

deny contains "ticket has no acceptance criteria" if {
	input.action == "delegate"
	not has_criteria
}

deny contains sprintf("WIP limit reached (%v)", [data.config.delegation.wip_limit]) if {
	input.action == "delegate"
	is_number(input.wip_in_progress)
	input.wip_in_progress >= data.config.delegation.wip_limit
}

deny contains "bounce limit reached: assign the story to the tech lead" if {
	input.action == "delegate"
	is_number(input.ticket.bounces)
	input.ticket.bounces >= data.config.delegation.bounce_limit
}

# publish: the diff's own paths decide, whatever the ticket said. A change
# that touches a path whose class isn't delegated is refused. Sub-plan 5
# passes the paths of the diff it is about to push.

deny contains "publish needs changed_paths as a list" if {
	input.action == "publish"
	not is_array(input.changed_paths)
}

deny contains sprintf("path %v is not a clean repo-relative path", [p]) if {
	input.action == "publish"
	is_array(input.changed_paths)
	some p in bad_paths_in(input.changed_paths)
}

deny contains sprintf("publish: path %v raises the risk class to %v", [p, r.class]) if {
	input.action == "publish"
	is_array(input.changed_paths)
	some p in input.changed_paths
	some r in rules_for(run_repo)
	path_matches(clean_path(p), r.prefix)
	not class_allowed(r.class)
}
