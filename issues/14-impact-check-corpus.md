---
title: "M4-14 · `rr impact`, `rr check`, corpus gelé et benchmarks"
labels: ["milestone:M4", "type:core", "quality"]
---

## Pourquoi
Clôture V1 : la valeur « impact » (le vrai différenciateur agent au quotidien),
le garde-fou `check`, et l'infrastructure de mesure sans laquelle aucune
itération de ranking n'est fiable (leçon du rollback d'abstention de Radar).

## Quoi
Trois blocs livrables séparément (a, b, c) dans cet ordre.

## Comment
### a) `rr impact` (sur change-set Git)
1. Delta : `gix diff HEAD..worktree` (ou `--base <ref>`), fichiers → symboles
   dont le span intersecte les hunks = « changed definitions ».
2. Arêtes directes depuis l'index (issue 06) : appels entrants (callers),
   imports entrants (dependents), à profondeur 2 max (`--depth`).
3. Tests probables : (i) fichiers de test référençant le nom du symbole
   changé (lexical, assumé), (ii) co-change Git : fichiers committés avec
   le fichier changé dans > 30 % de ses 50 derniers commits (gix log, calculé
   à la volée, caché par HEAD).
4. Sortie texte façon Radar observé (sections changed/edges/callers/tests)
   + compteurs `unresolved/ambiguous` affichés honnêtement + `--json`.

### b) `rr check`
Invariants : snapshot lisible et à la bonne version ; MAP.md présent et
api_hash cohérent ; ROUTES.md parsable, anchors `[ok]` existants ; budget
MAP respecté. Codes de sortie : 0 ok, 1 warnings (stale), 2 invariants
violés, 3 snapshot absent. À brancher en hook CI du repo lui-même.

### c) Corpus gelé + bench
1. `fixtures/corpus/` : 3 dépôts réels vendorés figés (petit/moyen/gros
   Rust) + `queries.yaml` étendu à 40 questions (comme Radar) avec anchors
   attendus vérifiés à la main.
2. `cargo test --release corpus` : top-3 ≥ 36/40, directs faux = 0 (bloquant).
3. Criterion : map froid/chaud, query p50/p95 (cible < 30 ms p95 sur le
   corpus 10 000 fichiers — parité avec le chiffre publié par Radar).
4. Les chiffres vont dans `BENCHMARKS.md` avec la commande exacte pour les
   reproduire — jamais un chiffre sans sa commande.

## Bonnes pratiques
- Impact n'invente jamais une arête : ce qui n'est pas résolu est compté,
  pas deviné (principe spec §10.6, validé par l'observation).
- Le corpus est FIGÉ : on n'y touche que par PR dédiée expliquant pourquoi.

## Critères d'acceptation
- [ ] Sur le fixture : édition de `verify_token` → `main` en caller,
      `users.rs` en dépendance, le test lexicalement lié listé (là où Radar échouait).
- [ ] `rr check` détecte un ROUTES.md corrompu (exit 2).
- [ ] Corpus 40 questions : seuils tenus, rapport imprimé.
- [ ] BENCHMARKS.md généré avec commandes reproductibles.
