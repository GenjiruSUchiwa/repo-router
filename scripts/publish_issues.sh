#!/usr/bin/env bash
# Publishes the local issues (issues/*.md) to GitHub, with milestones and labels.
# Prerequisites: gh CLI authenticated (`gh auth login`), repo already created and pushed.
# Usage: ./scripts/publish_issues.sh owner/repo
set -euo pipefail

REPO="${1:?usage: $0 owner/repo}"

# Milestones
for M in "M0 Bootstrap" "M1 Indexing" "M2 Query" "M3 Agent interface" "M4 Impact & quality"; do
  gh api "repos/$REPO/milestones" -f title="$M" >/dev/null 2>&1 || true
done

# Labels
gh label create "type:infra" --repo "$REPO" --color BFD4F2 2>/dev/null || true
gh label create "type:core" --repo "$REPO" --color 0E8A16 2>/dev/null || true
gh label create "type:agent-interface" --repo "$REPO" --color 5319E7 2>/dev/null || true
gh label create "type:quality" --repo "$REPO" --color FBCA04 2>/dev/null || true
gh label create "contract" --repo "$REPO" --color D93F0B 2>/dev/null || true
gh label create "differentiator" --repo "$REPO" --color E99695 2>/dev/null || true
gh label create "hard" --repo "$REPO" --color B60205 2>/dev/null || true

# Issues, in file order (01 → 14)
for f in issues/*.md; do
  title=$(sed -n 's/^title: "\(.*\)"$/\1/p' "$f")
  labels=$(sed -n 's/^labels: \[\(.*\)\]$/\1/p' "$f" | tr -d '"' | tr -d ' ')
  # milestone from the title prefix (M0..M4)
  case "$title" in
    M0-*) ms="M0 Bootstrap";; M1-*) ms="M1 Indexing";;
    M2-*) ms="M2 Query";;  M3-*) ms="M3 Agent interface";;
    M4-*) ms="M4 Impact & quality";; *) ms="";;
  esac
  body=$(awk 'BEGIN{fm=0} /^---$/{fm++; next} fm>=2{print}' "$f")
  # strip the milestone:* label from the gh label list
  labels=$(echo "$labels" | tr ',' '\n' | grep -v '^milestone:' | paste -sd, -)
  echo "→ $title"
  gh issue create --repo "$REPO" --title "$title" --body "$body" \
    ${labels:+--label "$labels"} ${ms:+--milestone "$ms"}
done
echo "Done: $(ls issues/*.md | wc -l) issues published."
