---
title: "M2-07 · `rr query` : routage exact + double contrat texte/JSON"
labels: ["milestone:M2", "type:core", "contract"]
---

## Pourquoi
Premier moment où un agent peut consommer l'outil. Le contrat de sortie est
un engagement public : on le fige ici et on ne le casse plus.

## Quoi
`rr query "<question>"` : détection d'identifiants explicites, lookup exact,
sortie texte (compatible dans l'esprit avec Radar observé) + `--json` stable.

## Comment
1. Détection d'identifiant explicite dans la requête : regex conservatrice
   (`[A-Za-z_][A-Za-z0-9_]*` contenant `_` OU casse mixte OU présent tel quel
   dans l'index exact ; chemins `a/b.rs`, formes `Foo::bar`, `x.y`).
2. Lookup `exact[name]` :
   - 1 résultat → réponse directe ;
   - N résultats → départage par overlap des autres termes de la requête
     avec chemin/qualified ; si toujours ambigu → candidats (max 3) ;
   - 0 → pipeline lexical (issue 08).
3. Contrat texte (stdout, rien d'autre) :
   ```text
   FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
   ```
   Ambigu : `source candidates:` + une ligne par anchor. Introuvable :
   `NO ANCHOR (index has no match); try: rr map` — divergence assumée avec
   Radar qui dumpe la carte (observation §4) : notre repli reste minuscule.
4. Contrat `--json` (schéma versionné, champ `v: 1`) :
   ```json
   {"v":1,"result":"direct","anchor":{"path":"src/auth/token.rs","symbol":"verify_token","lines":[9,15]},"confidence":1.0}
   ```
   Variantes `result`: `direct` | `candidates` | `none`. Écrire le JSON Schema
   dans `docs/query.schema.json`, testé en CI contre la sortie réelle.
5. Codes de sortie : 0 = direct, 2 = candidats, 3 = aucun, 1 = erreur
   d'exécution (divergence assumée : Radar renvoie 0 partout, un agent
   scripté mérite mieux).

## Bonnes pratiques
- Le contrat texte et le JSON sortent de la MÊME structure interne
  (`QueryResult`) via deux renderers — jamais deux chemins de calcul.
- Tests de contrat : fichiers `tests/contract/*.txt` comparés verbatim.

## Critères d'acceptation
- [ ] `rr query verify_token` → anchor direct, exit 0.
- [ ] `rr query session` (fixture) → 2 candidats, exit 2.
- [ ] `--json` valide contre le schéma en CI.
- [ ] Latence à chaud < 10 ms sur le corpus 10 000 fichiers.
