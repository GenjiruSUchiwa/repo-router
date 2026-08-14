---
title: "M2-09 · `--source` : span borné, vérifié par hash, refus si stale"
labels: ["milestone:M2", "type:core", "contract"]
---

## Pourquoi
La promesse centrale : jamais de source périmée. L'observation de Radar (§5)
a tranché notre débat de spec : refuser + orienter vers refresh est plus sûr
et plus simple que re-parser à chaud. On copie ce choix, avec une option de
relocalisation par diff en bonus différenciant.

## Quoi
`rr query ... --source` et le module `rr-core::verify`.

## Comment
1. Vérification : OID actuel du fichier (via rr-git, gratuit si working tree
   propre) vs `anchor.indexed_oid`.
   - identiques → lire uniquement les lignes du span (pas tout le fichier
     en mémoire si évitable), borner à `MAX_SOURCE_LINES = 120` avec marqueur
     `SOURCE TRUNCATED (N more lines)` le cas échéant ;
   - différents → refus :
     ```text
     STALE SOURCE (no content returned): src/auth/token.rs changed since indexing; run `rr refresh`
     ```
     exit 4 (contrairement à Radar qui renvoie 0 — un agent doit pouvoir
     brancher sans parser le texte).
2. Option `--relocate` (bonus, peut glisser en M4) : si stale, calculer le
   diff blob indexé ↔ contenu actuel (gix diff), mapper les lignes du span
   à travers les hunks ; si le mapping est net (span entier dans une zone
   inchangée ou décalée d'un offset constant) → servir le span relocalisé
   marqué `SOURCE SPAN (relocated)` ; sinon refus normal. Jamais par défaut.
3. Sortie texte (contrat) :
   ```text
   FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
   SOURCE SPAN (verified): src/auth/token.rs:9-15
   SOURCE COMPLETE
   ---
   <code>
   ```
   `--json` : mêmes données + `"verified": true|"relocated"|false`.

## Bonnes pratiques
- Le verify relit le fichier au moment T de la réponse — aucune donnée du
  snapshot ne part chez l'agent sans revalidation (spec §5.2, mot pour mot).
- Test de course : modifier le fichier entre le lookup et le read → le hash
  final fait foi (relire l'OID APRÈS lecture des bytes servis).

## Critères d'acceptation
- [ ] Édition sans refresh → STALE, aucun contenu, exit 4.
- [ ] Après refresh → span correct relocalisé (test décalage 2 lignes).
- [ ] Span > 120 lignes → tronqué avec marqueur.
- [ ] `--relocate` sert le span après insertion de commentaires en tête.
