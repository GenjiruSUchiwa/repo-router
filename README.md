# repo-router

**`rr`** — un navigateur de dépôt pour agents de code (Claude Code, etc.), inspiré du comportement public de [Radar](https://radar.dev). Il indexe un dépôt (Tree-sitter + fingerprints lexicaux), puis répond aux questions de navigation (« où est défini X ? », « qui appelle Y ? ») avec un **minimum de contexte** : une route précise vers la source, pas un dump de fichiers.

> Réimplémentation ouverte et cross-platform, écrite à partir du comportement publiquement documenté et observé de Radar — pas de son code source, qui reste privé. Voir [`docs/SPEC.md`](docs/SPEC.md) et [`docs/OBSERVATIONS.md`](docs/OBSERVATIONS.md).

## Pourquoi

Les agents de code brûlent leur fenêtre de contexte à `grep` et lire des fichiers entiers. `rr` vise l'inverse :

- **Contexte minimal par défaut** — une réponse tient en quelques lignes ; la source complète n'est lue que sur demande (`--source`), bornée et vérifiée par hash.
- **Exact avant flou** — routage exact (symbole, chemin) d'abord ; ranking lexical (BM25 par champs) ensuite ; **abstention** assumée quand la confiance est basse plutôt qu'une réponse plausible mais fausse.
- **Déterministe et incrémental** — index adressé par OID Git, snapshot atomique, `refresh` rapide git-gated. Pas de LLM dans la boucle de retrieval.
- **Contrats agent-friendly** — double sortie texte/JSON stable, `MAP.md`/`SYMBOLS.md` committés et greppables, `rr init` qui installe le contrat de navigation (dont un SKILL.md pour Claude Code).

## Commandes (cibles V1)

| Commande | Rôle |
|---|---|
| `rr map` | Indexe le dépôt (traversée gitignore-aware, Tree-sitter, fingerprints) |
| `rr query <q>` | Répond : définitions, références, imports — texte ou `--json` |
| `rr query --source` | Renvoie le span source exact, vérifié par hash, refus si stale |
| `rr refresh` | Met à jour l'index de façon incrémentale (git-gated) |
| `rr route` | Cache de routes résolues, committable |
| `rr impact <sym>` | Rayon d'impact d'un changement (appelants transitifs) |
| `rr check` | Garde-fou : cohérence index ↔ worktree |
| `rr init` | Installe le contrat de navigation dans le dépôt |
| `rr version` | Version + SHA git du build ✅ |

## État du projet

Bootstrap en cours. Le plan V1 tient en **14 issues sur 5 jalons** — voir les [issues](../../issues) et [jalons](../../milestones) :

- **M0 Bootstrap** — workspace, CI, hygiène ✅
- **M1 Indexation** — traversée, cache OID, Tree-sitter (Rust d'abord), fingerprints, snapshot
- **M2 Requête** — `query`, ranking + abstention, `--source`, `refresh`
- **M3 Interface agent** — `MAP.md`/`SYMBOLS.md`, `rr route`, `rr init`
- **M4 Impact & qualité** — `impact`, `check`, corpus gelé et benchmarks

## Développement

Rust stable. Workspace à trois crates :

```
crates/
  rr-core/   # modèle de données, indexes, ranking
  rr-git/    # traversée, OID, intégration git
  rr-cli/    # binaire `rr`
```

```sh
cargo build --release   # binaire dans target/release/rr
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings -D clippy::pedantic
cargo fmt --check
```

La CI (macOS arm64, Linux x64/arm64) exige fmt + clippy pedantic sans warning + tests. Les releases sont automatisées : conventional commits → [release-plz](https://release-plz.dev) (version, changelog, tag) → binaires multi-plateformes attachés à la GitHub Release.

## Licence

MIT.
