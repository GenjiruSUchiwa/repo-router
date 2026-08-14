---
title: "M1-02 · Traversée gitignore-aware et classification des fichiers"
labels: ["milestone:M1", "type:core"]
---

## Pourquoi
Indexer `node_modules/` ou `target/` détruit la pertinence et la vitesse.
La traversée est la fondation du pipeline `map` : elle décide ce qui existe.

## Quoi
Un module `rr-core::walk` qui produit la liste des fichiers source candidats,
avec langage détecté et drapeau `generated`.

## Comment
1. Utiliser la crate `ignore` (moteur de ripgrep) : respecte `.gitignore`,
   `.ignore`, exclusions globales — ne pas réécrire cette logique.
2. Exclusions par défaut en dur : `.git/`, `.rr/`, `node_modules/`, `target/`,
   `dist/`, `build/`, `.venv/`, `vendor/` (surchargables via `rr.toml` plus tard).
3. Détection de langage par extension d'abord (`.rs`, `.py`, `.ts`, `.tsx`) ;
   table extensible, pas de crate lourde de détection par contenu en V1.
4. Heuristique `generated` : chemin contient `generated`/`.pb.`/`_pb2.py`,
   ou première ligne contient `@generated` / `DO NOT EDIT`. Les fichiers
   generated sont indexés mais pénalisés au ranking (issue 08).
5. Parallélisme : `ignore::WalkBuilder::build_parallel()` avec un canal
   crossbeam vers le collecteur.

## Pseudo-code
```rust
pub struct SourceFile { pub path: RelPath, pub lang: Lang, pub generated: bool }

pub fn discover(root: &Path, cfg: &WalkCfg) -> Vec<SourceFile> {
    WalkBuilder::new(root)
        .standard_filters(true)          // gitignore, hidden, etc.
        .add_custom_ignore_rules(DEFAULT_EXCLUDES)
        .build_parallel()
        .collect_filter_map(|entry| classify(entry, cfg))
}
```

## Bonnes pratiques
- Chemins **relatifs à la racine repo**, normalisés `/`, dès cette couche —
  tout le reste du système ne voit jamais un chemin absolu (déterminisme
  des snapshots entre machines).
- Trier la sortie finale par chemin : ordre déterministe quel que soit le
  parallélisme (exigence spec §5.3).

## Critères d'acceptation
- [ ] Sur le fixture, découvre exactement les fichiers attendus, dans le même ordre à chaque run.
- [ ] Un pattern ajouté à `.gitignore` exclut immédiatement le fichier.
- [ ] Test : repo avec symlink cyclique → pas de boucle infinie.
