#!/usr/bin/env bash
# Condition control : Claude seul, AUCUN rr (pas sur PATH, pas d'artefacts)
set -u
source /tmp/lang-bench/tasks.sh
OUT=/tmp/lang-bench/results

run_control() {
  local repo="$1"
  local dir="/tmp/lang-bench/$repo"
  local run_dir="$OUT/control/$repo"
  mkdir -p "$run_dir"

  # Tâches
  local tasks=()
  case "$repo" in
    date-fns) tasks=("${TASKS_DATE_FNS[@]}") ;;
    Dapper)   tasks=("${TASKS_DAPPER[@]}") ;;
    gson)     tasks=("${TASKS_GSON[@]}") ;;
    serde)    tasks=("${TASKS_SERDE[@]}") ;;
    axios)    tasks=("${TASKS_AXIOS[@]}") ;;
    cobra)    tasks=("${TASKS_COBRA[@]}") ;;
  esac

  # Purge totale des artefacts rr du repo (état vanilla)
  (cd "$dir" && git checkout -- . 2>/dev/null; git clean -fdx >/dev/null 2>&1)

  for i in 0 1 2; do
    for run in 1 2; do
      local prompt="${tasks[$i]%%|*}"
      local expected="${tasks[$i]##*|}"
      local f="$run_dir/task${i}-run${run}"
      echo "  [control/$repo] task$i run$run"
      (cd "$dir" && claude -p "$prompt" --model sonnet --effort low \
        --dangerously-skip-permissions \
        --output-format text > "$f.txt" 2>/dev/null)
      echo "$expected" > "$f.expected"
      echo "control" > "$f.contract"
    done
  done
}

for repo in date-fns Dapper gson serde axios cobra; do
  echo "== $repo (control) =="
  run_control "$repo"
done
echo "=== FIN ==="
