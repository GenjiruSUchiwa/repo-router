---
title: "M1-03 · Adressage par OID Git et cache de faits partageable"
labels: ["milestone:M1", "type:core", "differentiator"]
---

## Pourquoi
C'est le changement d'approche n°1 face à Radar. Git calcule déjà un hash de
contenu (OID de blob) pour chaque fichier suivi. En clé de cache, l'OID rend
les faits parsés **partageables via une ref Git** : la CI indexe une fois,
l'équipe clone un index chaud. Radar (BLAKE3 local) ne peut pas faire ça.

## Quoi
`rr-git::oid` (calcul/lookup d'OID) et `rr-core::cache` (store de faits
clé → valeur, local d'abord, ref Git ensuite).

## Comment
1. Avec `gitoxide` (crate `gix`) : pour un fichier **non modifié** dans le
   working tree, lire l'OID directement depuis l'index Git (gratuit, zéro
   lecture du contenu). Pour un fichier modifié/non suivi, hasher en mémoire
   au format objet Git (`blob <len>\0<bytes>`, SHA-1 ou SHA-256 selon le repo).
2. Clé de cache complète : `(oid, lang, EXTRACTOR_VERSION, FACT_SCHEMA_VERSION)`.
   Bump de version d'extracteur ⇒ invalidation naturelle, aucun code de migration.
3. Store local V1 : fichiers `.rr/local/facts/<aa>/<oid>.bin` (bincode ou
   postcard), écrits par fichier temporaire + rename (atomicité).
4. V1.5 (issue séparée si besoin) : `rr cache push` / `rr cache pull` qui
   sérialise les faits dans un blob attaché à `refs/rr/facts` — ne bloque
   pas M1, mais la clé OID doit être en place dès maintenant.
5. Repo sans Git : fallback hash maison même format, drapeau `no_git` dans
   le snapshot (le partage est simplement indisponible).

## Pseudo-code
```rust
fn facts_for(file: &SourceFile, repo: &GitRepo, cache: &FactCache) -> Facts {
    let oid = repo.oid_of(file)          // index Git si propre
        .unwrap_or_else(|| hash_as_git_blob(read(file)));
    let key = CacheKey { oid, lang: file.lang, ext: EXTRACTOR_VERSION };
    cache.get(&key).unwrap_or_else(|| {
        let facts = extract(file);        // issue 04
        cache.put(&key, &facts);
        facts
    })
}
```

## Bonnes pratiques
- Ne jamais mettre le contenu du fichier dans le cache — uniquement des faits.
- Mesurer et logger (`--verbose`) le hit-rate du cache : c'est LA métrique
  de santé de l'incrémental.

## Critères d'acceptation
- [ ] Deuxième `rr map` sans modification : 100 % cache hits, 0 parse.
- [ ] `git mv` d'un fichier non modifié : cache hit (même OID).
- [ ] Édition d'un fichier : lui seul est re-parsé.
- [ ] Fonctionne dans un dossier sans `.git`.
