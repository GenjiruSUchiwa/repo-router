---
title: "M1-04 · Extraction Tree-sitter : définitions, références, imports (Rust d'abord)"
labels: ["milestone:M1", "type:core"]
---

## Pourquoi
C'est la matière première de tout le routage. La leçon de la spec §5.5 :
extraire le minimum utile, jeter l'AST tout de suite.

## Quoi
`rr-core::parser` : pour un fichier, produire `Facts { defs, refs, imports }`.
**Un seul langage en V1 : Rust** (celui du fixture et le nôtre — dogfooding).
Python et TypeScript arrivent en fin de M2, une fois le pipeline prouvé.

## Comment
1. Crates `tree-sitter` + `tree-sitter-rust` (versions épinglées — la version
   de grammaire fait partie de `EXTRACTOR_VERSION`, issue 03).
2. Écrire les extractions comme des **fichiers de requêtes `.scm`**
   (embarqués via `include_str!`), pas du code de traversée manuel :
   déclaratif, testable, et c'est le pattern standard de l'écosystème.
3. Extraire par définition : nom, nom qualifié si dérivable (module path),
   kind (fn/struct/enum/trait/impl-fn/const/mod), spans byte+ligne
   (début/fin), identifiants de signature, identifiants du corps, appels
   sortants (nom appelé + ligne), marqueur test (`#[test]`, chemin `tests/`).
4. Références : appels et `use` avec ligne. **Ne pas résoudre** ici — on
   stocke des noms, la résolution cheap arrive en issue 06/14 (leçon de
   l'observation : Radar assume 12 refs irrésolues et l'affiche).
5. Robustesse : un fichier qui ne parse pas ⇒ `Facts` dégradés
   (identifiants lexicaux seulement) + compteur d'erreurs, jamais un abort
   du map complet.

## Pseudo-code (requête .scm, extrait)
```scheme
(function_item name: (identifier) @def.name) @def.body
(call_expression function: [(identifier) @call.name
                            (field_expression field: (field_identifier) @call.name)])
(use_declaration argument: (_) @import.path)
```

## Bonnes pratiques
- Tests en or (golden tests) : fichier Rust d'entrée → snapshot YAML des
  faits attendus (crate `insta`). Toute évolution de grammaire devient un
  diff lisible en revue.
- Budget : viser < 1 ms/fichier moyen en release (bench dès maintenant,
  criterion, fichier de 300 lignes).

## Critères d'acceptation
- [ ] Sur `fixtures/rust-basic`, extrait les 10 symboles attendus avec les bons spans.
- [ ] `verify_token` porte bien `decode_jwt` et `now` en appels sortants.
- [ ] Fichier avec erreur de syntaxe : map complet réussit quand même.
- [ ] Golden tests insta en place.
