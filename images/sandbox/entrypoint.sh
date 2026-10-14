#!/bin/sh
# Kod sandbox entrypoint. Runs as root and drops to the unprivileged "agent"
# user for anything the agent or the repository controls. Results go to /out,
# which the agent user can't write.
# Usage: kod-entrypoint agent CMD... | collect | score
set -u
mode=$1
shift
as_agent() { env HOME=/home/agent setpriv --reuid=agent --regid=agent --init-groups -- "$@"; }
give_back() { chown -R "${KOD_HOST_UID:-0}:${KOD_HOST_GID:-0}" "$@"; }
# Ends everything the agent user started, so nothing can still write.
kill_agent() { while pgrep -u agent > /dev/null; do pkill -KILL -u agent; sleep 0.1; done; }
# Keeps the first KOD_LOG_CAP bytes in $1 and drains the rest, so the writer
# never sees a broken pipe.
cap() { head -c "$KOD_LOG_CAP" > "$1"; cat > /dev/null; }
# The agent and the tests write only to capped tmpfs mounts: /work (agent),
# /tmp and /home/agent. The root filesystem is read-only.
own_home() { chmod 755 /home/agent && chown agent:agent /home/agent; }

case "$mode" in
agent)
  trap 'give_back /out' EXIT
  # The host's workspace is read-only at /seed. Copy it into the tmpfs without
  # its owner (root without CAP_FOWNER can't chmod files it gave away), then
  # hand it to the agent.
  cp -R --preserve=mode,timestamps,links /seed/. /work/ || { echo "kod: the workspace does not fit in /work" >&2; exit 1; }
  chmod 755 /work && chown -R agent:agent /work && own_home || exit 1
  { claude --version 2>/dev/null || echo unknown; } | cap /out/runner_version
  # stdout to agent.json, stderr to agent.stderr, each capped. The exit status
  # is recorded before anything left running is killed, which also closes the
  # pipes.
  {
    { as_agent "$@" < /in/prompt.txt; echo $? > /out/exit_code; kill_agent; } 2>&1 >&3 3>&- | cap /out/agent.stderr
  } 3>&1 | cap /out/agent.json
  kill_agent
  # ponytail: metrics come from inside the container; read them from the host cgroup if they ever need to be tamper-proof
  cat /sys/fs/cgroup/memory.peak > /out/memory_peak
  awk '$1 == "usage_usec" { print $2 }' /sys/fs/cgroup/cpu.stat > /out/cpu_usec
  # The work tree for collect, bounded by the /work tmpfs. Renamed only when
  # complete, since tar reads a truncated archive without error.
  tar -C /work -cf /out/work.tar.part . && mv /out/work.tar.part /out/work.tar || rm -f /out/work.tar.part
  ;;
collect)
  # Runs no agent or repository code, and never uses the agent's .git.
  trap 'give_back /out' EXIT
  fail() { echo "collect failed: $1" >&2; exit 1; }
  export GIT_DIR=/tmp/collect.git GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null HOME=/nonexistent
  mkdir -p /tmp/base /tmp/work || fail mkdir
  # Every .git entry is dropped, so a nested repository can't hide its files.
  # Each tar runs on its own so a failure in the creating side is caught.
  # --no-same-owner: without CAP_FOWNER, root cannot chmod files it gave away.
  (cd /in && tar --exclude=.git -cf /tmp/base.tar .) || fail "archive base"
  (cd /tmp/base && tar --no-same-owner -xf /tmp/base.tar) || fail "copy base"
  # The agent container archived /work after the agent ended; it is missing
  # if that failed.
  (cd /tmp/work && tar --no-same-owner --exclude=.git -xf /agent/work.tar) || fail "copy work"
  git init -q || fail init
  # info/attributes overrides any .gitattributes the agent wrote: no filters,
  # no line-ending conversion, always a text diff.
  printf '* -text -filter -ident -working-tree-encoding diff\n' > "$GIT_DIR/info/attributes" || fail attributes
  # -f adds ignored files too, so a .gitignore can't hide a new file.
  GIT_WORK_TREE=/tmp/base git add -A -f || fail "add base"
  GIT_WORK_TREE=/tmp/base git -c user.name=kod -c user.email=kod@localhost commit -q --allow-empty -m base || fail commit
  GIT_WORK_TREE=/tmp/work git add -A -f || fail "add work"
  GIT_WORK_TREE=/tmp/work git diff --cached --binary --no-renames --no-ext-diff --no-textconv HEAD > /out/diff.patch || fail diff
  touch /out/collect_ok
  ;;
score)
  # /work is read-only here, so build scripts can't rewrite the held-back tests.
  trap 'give_back /out' EXIT
  mkdir -p /tmp/kod && chown -R agent:agent /tmp && own_home || exit 1
  # Nothing the tests started may touch the report after kill_agent.
  { as_agent env KOD_JUNIT=/tmp/kod/junit.xml sh -c "$KOD_TEST_COMMAND" 2>&1; echo $? > /out/score_exit; kill_agent; } | cap /out/score.log
  if [ -f /tmp/kod/junit.xml ] && [ ! -L /tmp/kod/junit.xml ]; then cap /out/junit.xml < /tmp/kod/junit.xml; fi
  ;;
*)
  echo "unknown mode: $mode" >&2
  exit 2
  ;;
esac
