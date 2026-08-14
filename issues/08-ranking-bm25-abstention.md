---
title: "M2-08 · Ranking BM25 par champs + seuils d'abstention calibrés"
labels: ["milestone:M2", "type:core", "hard"]
---

## Pourquoi
La partie la plus difficile du projet. Radar a publié un échec de calibration
d'abstention (4 anchors faux sur 9 après une calibration qui semblait sûre)
et a fait un rollback. On prend le problème au sérieux dès la conception :
**pas de seuil sans corpus de test gelé** (issue 14 fournit le harnais ;
un mini-corpus de 20 questions est créé ici même).

## Quoi
Pipeline lexical pour requêtes sans identifiant exact : candidats → score
BM25F → décision directe/candidats/aucun.

## Comment
1. Candidats : union des postings des termes de requête (tous champs),
   plafonnée à 64 par doc-frequency croissante (termes rares d'abord).
2. Score BM25F sur documents synthétiques par symbole (les fingerprints,
   PAS le fichier source — spec §11.4), champs pondérés :
   nom 8, qualified 5, chemin 5, signature 4, appelés 3, corps 1.5.
   Pénalité ×0.5 si `generated`. Bonus léger si kind ∈ {fn, method} quand
   la requête contient un verbe.
3. Décision (valeurs initiales, à recalibrer sur corpus, jamais en dur
   ailleurs que dans `ranking.rs::THRESHOLDS`) :
   - direct ssi `score[0] > T_abs` ET `score[0]/score[1] > 1.6` ;
   - sinon candidats top-3 ;
   - aucun si `score[0] < T_min`.
   Le ratio top1/top2 est plus robuste que le score absolu — c'est la marge
   qui prédit la justesse, pas la magnitude.
4. Tie-break déterministe final : (score desc, SymbolId asc). Deux runs =
   même sortie, toujours.
5. Mini-corpus `fixtures/queries.yaml` : 20 questions → anchor attendu.
   `cargo test ranking_corpus` échoue si top-3 < 18/20 ou si un direct est faux.
   **Un direct faux est pire qu'une abstention** : le test pondère ainsi.

## Pseudo-code
```rust
let mut scored: Vec<_> = candidates(q, 64)
    .map(|s| (bm25f(q, s, &weights), s))
    .collect();
scored.sort_by(|a,b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
match decide(&scored, &THRESHOLDS) { Direct(..) | Candidates(..) | None }
```

## Bonnes pratiques
- Chaque changement de poids = un commit dédié avec le diff du score corpus
  dans le message. L'historique Git devient le journal de calibration.
- `rr query --explain` (flag caché) : imprime les features par candidat —
  indispensable pour déboguer le ranking sans deviner.

## Critères d'acceptation
- [ ] Corpus 20 questions : ≥ 18 top-3, 0 direct faux.
- [ ] « where is token verification handled? » → direct `verify_token`.
- [ ] « security logic » (hors vocabulaire) → candidats ou aucun, JAMAIS un direct faux.
- [ ] Déterminisme : 100 runs, sorties identiques.
