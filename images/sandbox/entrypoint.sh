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

case "$mode" in
agent)
  trap 'give_back /work /out' EXIT
  chown -R agent:agent /work
  claude --version > /out/runner_version 2>/dev/null || echo unknown > /out/runner_version
  as_agent "$@" < /in/prompt.txt > /out/agent.json 2> /out/agent.stderr
  echo $? > /out/exit_code
  # ponytail: metrics come from inside the container; read them from the host cgroup if they ever need to be tamper-proof
  cat /sys/fs/cgroup/memory.peak > /out/memory_peak
  awk '$1 == "usage_usec" { print $2 }' /sys/fs/cgroup/cpu.stat > /out/cpu_usec
  ;;
collect)
  # Runs no agent or repository code, and never uses the agent's .git.
  trap 'give_back /out' EXIT
  fail() { echo "collect failed: $1" >&2; exit 1; }
  export GIT_DIR=/tmp/collect.git GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null HOME=/nonexistent
  mkdir -p /tmp/base /tmp/work || fail mkdir
  # Every .git entry is dropped, so a nested repository can't hide its files.
  # Each tar runs on its own so a failure in the creating side is caught.
  (cd /in && tar --exclude=.git -cf /tmp/base.tar .) || fail "archive base"
  (cd /tmp/base && tar -xf /tmp/base.tar) || fail "copy base"
  (cd /work && tar --exclude=.git -cf /tmp/work.tar .) || fail "archive work"
  (cd /tmp/work && tar -xf /tmp/work.tar) || fail "copy work"
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
  trap 'give_back /tmp /out' EXIT
  mkdir -p /tmp/kod && chown -R agent:agent /tmp
  as_agent env KOD_JUNIT=/tmp/kod/junit.xml sh -c "$KOD_TEST_COMMAND" > /out/score.log 2>&1
  echo $? > /out/score_exit
  # Nothing the tests started may touch the report after this point.
  while pgrep -u agent > /dev/null; do pkill -KILL -u agent; sleep 0.1; done
  if [ -f /tmp/kod/junit.xml ] && [ ! -L /tmp/kod/junit.xml ]; then cp /tmp/kod/junit.xml /out/junit.xml; fi
  ;;
*)
  echo "unknown mode: $mode" >&2
  exit 2
  ;;
esac
