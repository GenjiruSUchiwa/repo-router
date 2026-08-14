---
title: "M1-06 · Index exact + postings lexicaux + snapshot atomique"
labels: ["milestone:M1", "type:core"]
---

## Pourquoi
Transforme les faits en structures interrogeables. Clôt le jalon M1 :
`rr map` devient utilisable de bout en bout.

## Quoi
`rr-core::index` : construction en mémoire, sérialisation `.rr/local/snapshot.bin`,
et la commande CLI `rr map` qui orchestre issues 02→06.

## Comment
1. Structures (IDs u32 partout, cf. spec §8) :
   - `exact: HashMap<TermId /*nom exact*/, SmallVec<SymbolId>>`
   - `qualified: HashMap<TermId, SmallVec<SymbolId>>`
   - `postings: HashMap<TermId, RoaringBitmap /*SymbolId*/>` par champ
     (nom / chemin / signature / corps / appelés) — 5 maps, pas une map de maps.
   - `files: Vec<FileRecord>`, `symbols: Vec<SymbolRecord>` (arena, index = ID).
2. Résolution cheap des relations : un appel `foo()` est résolu ssi exactement
   un symbole nommé `foo` existe dans le même fichier ou module ; sinon rester
   nom irrésolu + compteur (affiché par `rr map --verbose`).
3. Sérialisation : `postcard` ou `bincode` + en-tête
   `{ magic, SCHEMA_VERSION, repo_head_oid, created_at }`. Version différente
   au chargement ⇒ rebuild silencieux, jamais de migration.
4. Écriture atomique : temp file dans le même dossier + `rename` (spec §10.8).
5. `rr map` : sortie une ligne, façon Radar observé :
   `rr map — 42 files, 310 symbols, 12 unresolved refs, 38 ms (cache 95%)`.

## Pseudo-code
```rust
fn build(root: &Path) -> Snapshot {
    let files = discover(root, &cfg);                     // 02
    let facts: Vec<_> = files.par_iter()
        .map(|f| (f, facts_for(f, &repo, &cache)))        // 03+04
        .collect();
    let mut ix = Index::default();
    for (f, facts) in facts { ix.add(f, facts, &mut interner /*05*/); }
    ix.resolve_unambiguous();
    ix.freeze_sorted()          // tri déterministe de toutes les postings
}
```

## Bonnes pratiques
- `freeze_sorted()` trie chaque SmallVec/bitmap : deux builds du même arbre
  donnent des snapshots **byte-identiques** (test d'or du déterminisme).
- Budget mémoire : pas de String dupliquée, tout passe par l'interner.

## Critères d'acceptation
- [ ] `rr map` deux fois de suite → snapshots byte-identiques.
- [ ] Snapshot du fixture < 50 Ko.
- [ ] Version de schéma bumpée ⇒ rebuild auto sans erreur.
- [ ] 10 000 fichiers générés (script fourni) : map à froid < 5 s, à chaud < 300 ms.
