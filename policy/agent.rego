# Agent operations through the gateways: model requests (routing policy),
# vendor-login model requests, tracker and docs reads and writes, draft docs
# and publish. Callers: sub-plan 2 (LLM gateway) and sub-plan 5 (tool gateway).
package kod

agent_actions := {"model_request", "subscription_model_request", "read", "tracker_write", "draft_doc_write", "publish"}

write_actions := {"tracker_write", "draft_doc_write", "publish"}

class_actions := {"model_request", "subscription_model_request", "publish"}

is_agent if input.actor.kind == "agent"

# A run is live only when it says canceled: false.
live_run if input.run.canceled == false

has_classes if {
	is_array(input.run.data_classes)
	count(input.run.data_classes) > 0
}

# Every class the run read is a known class.
run_classes_known if {
	has_classes
	every c in input.run.data_classes {
		data.config.routing.classes[c]
	}
}

deny contains "only an agent can request this" if {
	input.action in agent_actions
	not is_agent
}

deny contains "the run is canceled" if {
	input.action in write_actions
	input.run.canceled == true
}

deny contains "the run has no data class" if {
	input.action in class_actions
	not has_classes
}

deny contains sprintf("unknown data class: %v", [c]) if {
	input.action in class_actions
	has_classes
	some c in input.run.data_classes
	not data.config.routing.classes[c]
}

# model_request: the endpoint must be allowed for every class the run read.

endpoint_allowed(c) if input.endpoint in data.config.routing.classes[c].endpoints

grant if {
	input.action == "model_request"
	is_agent
	input.run.auth_mode in {"api_key", "cloud"}
	run_classes_known
	every c in input.run.data_classes {
		endpoint_allowed(c)
	}
}

model_auth_ok if input.run.auth_mode in {"api_key", "cloud"}

deny contains "model_request needs auth_mode api_key or cloud" if {
	input.action == "model_request"
	input.run.auth_mode
	not model_auth_ok
}

deny contains sprintf("endpoint %v is not allowed for data class %v", [input.endpoint, c]) if {
	input.action == "model_request"
	has_classes
	some c in input.run.data_classes
	data.config.routing.classes[c]
	not endpoint_allowed(c)
}

# subscription_model_request: the credential exception. Offline only, a
# business plan, and the seat's workspace must be the one recorded.

sub_runner := data.config.subscription.runners[input.runner]

sub_context_ok if input.context in data.config.subscription.contexts

sub_plan_ok if input.seat.plan in sub_runner.plans

sub_class_ok(c) if c in data.config.subscription.classes

seat_ok if {
	sub_plan_ok
	is_string(sub_runner.workspace)
	named(input.seat.workspace)
	input.seat.workspace == sub_runner.workspace
	named(input.seat.account)
}

grant if {
	input.action == "subscription_model_request"
	is_agent
	sub_context_ok
	seat_ok
	run_classes_known
	every c in input.run.data_classes {
		sub_class_ok(c)
	}
}

deny contains sprintf("subscription mode is allowed only in: %v", [concat(", ", data.config.subscription.contexts)]) if {
	input.action == "subscription_model_request"
	not sub_context_ok
}

deny contains sprintf("runner %v has no subscription seat", [input.runner]) if {
	input.action == "subscription_model_request"
	not sub_runner
}

deny contains sprintf("plan %v is not a business plan for runner %v", [input.seat.plan, input.runner]) if {
	input.action == "subscription_model_request"
	sub_runner
	not sub_plan_ok
}

deny contains sprintf("no approved workspace is recorded for runner %v", [input.runner]) if {
	input.action == "subscription_model_request"
	sub_runner
	not is_string(sub_runner.workspace)
}

deny contains "the seat belongs to a different workspace" if {
	input.action == "subscription_model_request"
	is_string(sub_runner.workspace)
	input.seat.workspace != sub_runner.workspace
}

deny contains sprintf("data class %v is not allowed in subscription mode", [c]) if {
	input.action == "subscription_model_request"
	has_classes
	some c in input.run.data_classes
	data.config.routing.classes[c]
	not sub_class_ok(c)
}

# read: the run's own ticket, the rework requests Kod linked to it (the
# run's linked_tickets), and the docs the ticket links.

grant if {
	input.action == "read"
	is_agent
	input.target.kind == "ticket"
	named(input.run.ticket)
	input.target.id == input.run.ticket
}

grant if {
	input.action == "read"
	is_agent
	input.target.kind == "ticket"
	named(input.target.id)
	is_array(input.run.linked_tickets)
	input.target.id in input.run.linked_tickets
}

grant if {
	input.action == "read"
	is_agent
	input.target.kind == "doc"
	named(input.target.id)
	is_array(input.run.linked_docs)
	input.target.id in input.run.linked_docs
}

# tracker_write: comment or transition on the run's ticket or a request
# linked to it; create a request linked to the run's ticket.

own_or_linked(t) if {
	named(t)
	t == input.run.ticket
}

own_or_linked(t) if {
	named(t)
	is_array(input.run.linked_tickets)
	t in input.run.linked_tickets
}

grant if {
	input.action == "tracker_write"
	is_agent
	live_run
	input.kind in {"comment", "transition"}
	own_or_linked(input.target.ticket)
}

grant if {
	input.action == "tracker_write"
	is_agent
	live_run
	input.kind == "create_linked"
	named(input.run.ticket)
	input.target.parent == input.run.ticket
}

tracker_write_kind if input.kind in {"comment", "transition", "create_linked"}

deny contains sprintf("unknown tracker write: %v", [input.kind]) if {
	input.action == "tracker_write"
	not tracker_write_kind
}

# draft_doc_write: the run's own draft, in the Draft folder.

grant if {
	input.action == "draft_doc_write"
	is_agent
	live_run
	input.target.folder == "draft"
	named(input.run.id)
	input.target.owner_run == input.run.id
}

# publish: the run's repo, branch agent/<ticket>-<run>, a draft PR, and
# only data classes cleared for publish.

run_branch := sprintf("agent/%s-%s", [input.run.ticket, input.run.id]) if {
	named(input.run.ticket)
	named(input.run.id)
}

publish_class_ok(c) if c in data.config.publish.cleared_classes

grant if {
	input.action == "publish"
	is_agent
	live_run
	named(input.run.repo)
	input.target.repo == input.run.repo
	input.target.branch == run_branch
	input.target.draft == true
	run_classes_known
	every c in input.run.data_classes {
		publish_class_ok(c)
	}
}

deny contains "publish target must be the run's repo" if {
	input.action == "publish"
	input.target.repo != input.run.repo
}

deny contains sprintf("publish branch must be %v", [run_branch]) if {
	input.action == "publish"
	input.target.branch != run_branch
}

deny contains "publish must open a draft PR" if {
	input.action == "publish"
	input.target.draft != true
}

deny contains sprintf("data class %v is not cleared for publish", [c]) if {
	input.action == "publish"
	has_classes
	some c in input.run.data_classes
	data.config.routing.classes[c]
	not publish_class_ok(c)
}
