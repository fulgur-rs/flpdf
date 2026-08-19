#!/usr/bin/env bash
# CI helper: install apt packages with a hard timeout and retries.
#
# CI's apt mirrors (archive.ubuntu.com in particular) have repeatedly hung
# mid-download with the connection open but no further data arriving, well
# past apt's own Acquire::http::Timeout / Acquire::https::Timeout options --
# those only bound idle time on a connection apt considers alive, and do not
# reliably fire on this failure shape. Wrap the whole operation in a hard
# shell-level `timeout` instead, so a stalled mirror is killed and retried
# rather than hanging for the runner's full job timeout. `--kill-after`
# escalates to SIGKILL if apt/sudo/a child ignores the initial SIGTERM,
# since `timeout`'s own default signal is not guaranteed to be honored.
set -euo pipefail

apt_opts=(
  -o Acquire::Retries=3
  -o Acquire::http::Timeout=15
  -o Acquire::https::Timeout=15
)

for attempt in 1 2 3; do
  if timeout --kill-after=10 90 sudo apt-get "${apt_opts[@]}" update \
    && timeout --kill-after=10 180 sudo apt-get "${apt_opts[@]}" install -y "$@"; then
    exit 0
  fi
  echo "apt-get attempt $attempt failed or timed out; retrying..." >&2
  sleep 5
done

echo "apt-get failed after 3 attempts" >&2
exit 1
