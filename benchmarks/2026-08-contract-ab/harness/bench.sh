#!/usr/bin/env bash
# Benchmark multi-langage : MAP-first vs radar-style
# 6 repos × 3 tâches × 2 runs × 2 contrats = 72 sessions
set -u
source /tmp/lang-bench/tasks.sh

OUT=/tmp/lang-bench/results
mkdir -p "$OUT/usage-logs"

run_repo() {
  local repo="$1" bin="$2" contract="$3"
  local dir="/tmp/lang-bench/$repo"
  local run_dir="$OUT/$contract/$repo"
  mkdir -p "$run_dir"

  # Tableau des tâches : selection par nom de repo (pas de nameref, bash 3.2)
  local tasks=()
  case "$repo" in
    date-fns) tasks=("${TASKS_DATE_FNS[@]}") ;;
    Dapper)   tasks=("${TASKS_DAPPER[@]}") ;;
    gson)     tasks=("${TASKS_GSON[@]}") ;;
    serde)    tasks=("${TASKS_SERDE[@]}") ;;
    axios)    tasks=("${TASKS_AXIOS[@]}") ;;
    cobra)    tasks=("${TASKS_COBRA[@]}") ;;
  esac

  # Installer le contrat + mapper (le binaire porte le contrat)
  (cd "$dir" && "$bin" init --root . >/dev/null 2>&1; "$bin" map >/dev/null 2>&1)

  # Le hook loggue dans le log commun
  export RR_BENCH_LOG="$OUT/usage-logs"

  for i in 0 1 2; do
    for run in 1 2; do
      local prompt="${tasks[$i]%%|*}"
      local expected="${tasks[$i]##*|}"
      local sid=$(uuidgen | tr '[:upper:]' '[:lower:]')
      local f="$run_dir/task${i}-run${run}"
      echo "  [$contract/$repo] task$i run$run"
      (cd "$dir" && claude -p "$prompt" --model sonnet --effort low \
        --dangerously-skip-permissions \
        --session-id "$sid" \
        --output-format text \
        > "$f.txt" 2>/dev/null)
      echo "$expected" > "$f.expected"
      echo "$sid" > "$f.session"
      echo "$contract" > "$f.contract"
    done
  done
}

for repo in date-fns Dapper gson serde axios cobra; do
  echo "== $repo =="
  run_repo "$repo" /tmp/lang-bench/bins/rr-mapfirst mapfirst
  run_repo "$repo" /tmp/lang-bench/bins/rr-radarstyle radarstyle
done
echo "=== FIN ==="
