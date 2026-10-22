# Audit log runbook

The `kod-audit` CLI prints one JSON line per command and exits non-zero on any failure. Subcommands: `checkpoint`, `verify`, `purge`, `capture-checkpoints`. Global flags: `--db` (default `/var/lib/kod/audit.db`) and `--checkpoints` (required).

Install: `cargo install --path crates/kod-audit --locked`. The CLI never creates a database; the gateways and the dispatcher create it when they open it, so start one of them (or the eval run) first. A deleted or replaced database or `-wal` file makes writes fail closed.

Prerequisites: `kod-audit` on both hosts, the `sqlite3` CLI on the audit host, and an ssh key that lets the checkpoint account log in to the audit host as `kod`.

`kod-audit` runs on the audit host as the `kod` user. The database is `/var/lib/kod/audit.db`; the directory is mode 0700 and the database, `-wal` and `-shm` files are 0600. Checkpoints go to `/mnt/kod-checkpoints`, a mount owned by the checkpoint account: `kod` can create files there but can't change or delete them.

Timers on the audit host (systemd, each with `OnFailure=` alerting the owner):

```
# kod-audit-checkpoint.timer: OnCalendar=*:0/5
ExecStart=/usr/local/bin/kod-audit --checkpoints /mnt/kod-checkpoints checkpoint
# kod-audit-verify.timer: OnCalendar=hourly (early warning only)
ExecStart=/usr/local/bin/kod-audit --checkpoints /mnt/kod-checkpoints verify
# kod-audit-purge.timer: OnCalendar=daily, one purge per record type in the
# compliance profile, refused and recorded while it is a placeholder;
# every type is tried and the unit fails at the end if any failed
ExecStart=/bin/sh -c 'rc=0; for t in run_audit prompts model_outputs; do /usr/local/bin/kod-audit --checkpoints /mnt/kod-checkpoints purge --policy /etc/kod/policy --record-type "$t" || rc=1; done; exit $rc'
```

The authoritative check runs on the checkpoint host, under the checkpoint account. It freezes its trusted checkpoint set **before** requesting the database snapshot. Checkpoints published later belong to the next check; a copied database must cover every frozen checkpoint, so actual truncation is still refused. Hourly, with `OnFailure=` alerting the owner:

```
set -eu
umask 077
verify_work=$(mktemp -d /srv/kod-verify/run.XXXXXX)
kod-audit --checkpoints /srv/kod-checkpoints capture-checkpoints --output "$verify_work/checkpoints"
ssh kod@audit-host rm -f /var/lib/kod/snap.db
printf "VACUUM INTO '/var/lib/kod/snap.db';" | ssh kod@audit-host sqlite3 /var/lib/kod/audit.db
scp kod@audit-host:/var/lib/kod/snap.db "$verify_work/audit.db"
kod-audit --db "$verify_work/audit.db" --checkpoints "$verify_work/checkpoints" verify --released-policies /srv/kod-policies
touch "$verify_work/VERIFIED"
```

`/srv/kod-policies` holds every released `policy/` directory, one subdirectory per version (for example the release tag), copied there when it is released and never deleted: each `retention` record is checked against the policy it names. Owner and auditors read only run directories with a `VERIFIED` marker, never the live database. Run this job as one systemd oneshot so remote snapshot creation cannot overlap another invocation.

Retention currently means **logical payload clearing**, not storage erasure. `payloads_cleared` counts SQL values set to NULL; old bytes can remain in WAL frames, freed pages, the remote `snap.db`, these run directories, and backups. Keep the placeholder profile refusing deletions until the owner approves both retention values and a documented erasure/copy-expiry procedure. That procedure must name the SQLite secure-delete/checkpoint or compaction strategy and its verification, the owner and expiry of every snapshot/backup, and how failed or offline copies are handled. A WAL checkpoint alone does not erase freed pages or copies. Expire failed run directories too; retain checkpoint hashes separately from payload-bearing copies. Test storage recovery and copy expiry with synthetic markers before enabling a non-placeholder profile. This plan does not implement or claim physical erasure.

Checkpoints that only the audit host reads catch accidents, not someone with root there: root can mount a forged directory over `/mnt/kod-checkpoints`. Until the checkpoint account and this job exist, the integrity exit criterion is not met.

A failed `verify`, on either host, means the log was changed. Stop publishing (kill switch), keep the database and the checkpoint directory as they are, and tell the owner. The error names the first bad entry. A `retention` record that is malformed also blocks every later purge (the store's trigger can't read it), which fails closed; `verify` names it, and it is handled as tampering like any other.
