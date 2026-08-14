---
title: "M3-12 · Cache de routes résolues, committable (`rr route`)"
labels: ["milestone:M3", "type:agent-interface", "differentiator"]
---

## Pourquoi
La meilleure idée de Radar (ROUTES.md auto-amorcé + `route add`) enfermée
dans un dossier gitignoré : chaque développeur réapprend tout. Changement
d'approche n°3 : en faire une **mémoire d'équipe versionnée**. Six mois de
questions réelles → réponses vérifiées, invalidées proprement par api_hash.

## Quoi
`.rr/ROUTES.md` committé + sous-commandes `rr route add|find|list` +
consultation automatique par `rr query` avant le ranking.

## Comment
1. Format ligne (stable, trié par mots-clés) :
   `[state] mots clés | file#symbol | api_hash | hits`
   états : `auto` (semé par rr depuis les noms de symboles), `ok` (validé —
   ajouté par un agent/humain et anchor vérifié), `stale` (api_hash ≠ courant).
2. `rr route add "<tâche>" <file#symbol>` : normalise la tâche (issue 05),
   VÉRIFIE que l'anchor existe dans l'index courant (refus sinon — pas de
   pollution), écrit `[ok]`, trie le fichier.
3. `rr query` : avant le pipeline lexical, chercher un overlap fort
   (≥ 2/3 des termes de requête) avec une route `[ok]` fraîche → réponse
   directe marquée `(from route cache)` dans le JSON, `hits += 1`.
   Les routes `[auto]` ne court-circuitent pas le ranking (elles ne font
   que booster, +2.0 au score) ; les `stale` sont ignorées.
4. `rr refresh` re-marque les états selon l'api_hash courant et re-seed les
   `[auto]` manquantes. Jamais de suppression automatique d'une `[ok]`
   (même stale : un humain la retire ou la re-valide).
5. Politique local vs committé : tout dans le même fichier committé —
   simplicité d'abord ; si le bruit de diff gêne, on scindera plus tard.

## Bonnes pratiques
- Le fichier reste lisible ET éditable à la main : c'est une feature, pas
  un format interne (revue en PR des routes ajoutées par les agents !).
- Tri déterministe systématique après chaque écriture.

## Critères d'acceptation
- [ ] `route add` avec anchor inexistant → refus, exit 1.
- [ ] Query matchant une route `[ok]` → réponse < 5 ms, hits incrémenté.
- [ ] Changement d'API → route passe `[stale]`, ignorée par query.
- [ ] Fichier stable au diff après opérations répétées.
