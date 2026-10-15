# Compliance profile (spec, "Configurable policies"). Several profiles can be
# active and the strictest wins: for retention, the longest period. A rule
# that reads a missing value denies. Caller: sub-plan 4 (audit store).
package kod

profiles := data.config.compliance.profiles

retention_known if {
	count(profiles) > 0
	every _, p in profiles {
		is_number(p.retention_days[input.record_type])
	}
}

grant if {
	input.action == "retention_delete"
	retention_known
	is_number(input.age_days)
	input.age_days >= max({p.retention_days[input.record_type] | some p in profiles})
}

deny contains "no compliance profile is active" if {
	input.action == "retention_delete"
	count(profiles) == 0
}

deny contains sprintf("compliance profile %v has no retention_days.%v", [name, input.record_type]) if {
	input.action == "retention_delete"
	some name, p in profiles
	not is_number(p.retention_days[input.record_type])
}
