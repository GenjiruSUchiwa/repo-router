#!/usr/bin/env bash
# Publie les issues locales (issues/*.md) sur GitHub, avec jalons et labels.
# Prérequis : gh CLI authentifié (`gh auth login`), repo déjà créé et push fait.
# Usage : ./scripts/publish_issues.sh owner/repo
set -euo pipefail

REPO="${1:?usage: $0 owner/repo}"

# Jalons
for M in "M0 Bootstrap" "M1 Indexation" "M2 Requête" "M3 Interface agent" "M4 Impact & qualité"; do
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

# Issues, dans l'ordre des fichiers (01 → 14)
for f in issues/*.md; do
  title=$(sed -n 's/^title: "\(.*\)"$/\1/p' "$f")
  labels=$(sed -n 's/^labels: \[\(.*\)\]$/\1/p' "$f" | tr -d '"' | tr -d ' ')
  # jalon depuis le préfixe du titre (M0..M4)
  case "$title" in
    M0-*) ms="M0 Bootstrap";; M1-*) ms="M1 Indexation";;
    M2-*) ms="M2 Requête";;  M3-*) ms="M3 Interface agent";;
    M4-*) ms="M4 Impact & qualité";; *) ms="";;
  esac
  body=$(awk 'BEGIN{fm=0} /^---$/{fm++; next} fm>=2{print}' "$f")
  # retirer le label milestone:* de la liste des labels gh
  labels=$(echo "$labels" | tr ',' '\n' | grep -v '^milestone:' | paste -sd, -)
  echo "→ $title"
  gh issue create --repo "$REPO" --title "$title" --body "$body" \
    ${labels:+--label "$labels"} ${ms:+--milestone "$ms"}
done
echo "Terminé : $(ls issues/*.md | wc -l) issues publiées."
