---
title: "M2-10 · `rr refresh` : chemin rapide git-gated"
labels: ["milestone:M2", "type:core"]
---

## Pourquoi
L'incrémental est ce qui rend l'outil invocable en boucle par un agent sans
friction. Radar l'a (`git-gated fast path`) ; le nôtre est plus simple grâce
au cache OID (issue 03) qui fait déjà 90 % du travail.

## Quoi
`rr refresh` : détecter le delta, re-parser le minimum, réécrire le snapshot.

## Comment
1. Fast path : comparer `snapshot.repo_head_oid` + statut working tree
   (gix status). Si HEAD identique et arbre propre → « 0 changed », exit 0,
   sans toucher au snapshot.
2. Delta : fichiers modifiés/ajoutés/supprimés/renommés depuis le snapshot
   (gix status + comparaison de la liste de fichiers du snapshot).
3. Reconstruire l'index À PARTIR du cache de faits : seuls les fichiers du
   delta passent par le parseur ; tous les autres sont des cache hits.
   V1 : reconstruction complète des postings en mémoire (rapide) ; ne PAS
   tenter la mise à jour in-place des bitmaps — complexité non justifiée
   tant que le rebuild à chaud < 300 ms sur 10 000 fichiers (mesuré en 06).
4. Sortie : `rr refresh — 1 reparsed, 41 cached, snapshot updated (12 ms)`.
5. `rr status` (10 lignes de code une fois refresh fait) : une ligne
   `git: dirty @ <sha> · snapshot: fresh|stale (N files) · unresolved: 12`.

## Bonnes pratiques
- Refresh est idempotent et sans danger : interrompu à tout moment, le
  snapshot précédent reste valide (écriture atomique de 06).
- L'agent ne doit JAMAIS avoir besoin de décider entre map et refresh :
  refresh fait la bonne chose, map = alias forçant full rebuild.

## Critères d'acceptation
- [ ] Arbre propre, HEAD inchangé : refresh < 5 ms, snapshot intact (mtime).
- [ ] 1 fichier édité : exactement 1 re-parse.
- [ ] Suppression d'un fichier : ses symboles disparaissent des résultats.
- [ ] `rr status` reflète correctement propre/sale/stale.
