---
title: "M0-01 · Bootstrap du workspace Cargo, CI et hygiène de base"
labels: ["milestone:M0", "type:infra"]
---

## Pourquoi
Tout le reste s'appuie dessus. Un workspace propre dès le départ évite les
refactorings de structure au milieu du développement, et une CI qui compile
sur les trois cibles garantit qu'on ne découvre pas les problèmes de
portabilité (notre argument n°1 : macOS ARM64) au moment de la release.

## Quoi
Créer le monorepo Cargo avec les crates vides mais compilables, le lint, la CI.

```text
repo-router/
├── Cargo.toml            # workspace
├── crates/
│   ├── rr-cli/           # binaire `rr` (clap)
│   ├── rr-core/          # parser / facts / index / query / verify / cache
│   └── rr-git/           # OIDs, refs, diff (gitoxide)
├── fixtures/             # dépôts de test (issue 14 les gèlera)
└── benches/
```

## Comment
1. `cargo new --lib crates/rr-core`, `crates/rr-git`; `cargo new crates/rr-cli`.
2. Workspace `Cargo.toml` : `resolver = "2"`, profil release `lto = true`,
   `codegen-units = 1`, `strip = "symbols"`, `panic = "abort"`.
3. `rr-cli` : clap v4 en mode derive, une commande `rr version` qui affiche
   version + git sha compilé (via `build.rs` + `vergen` ou équivalent simple).
4. CI GitHub Actions : matrix `macos-14` (ARM), `ubuntu-latest` (x86_64),
   `ubuntu-24.04-arm` ; étapes `cargo fmt --check`, `cargo clippy -- -D warnings`,
   `cargo test`, `cargo build --release`.
5. Gérer SIGPIPE proprement dès maintenant (leçon de l'observation §9.6) :
   dans `main()`, remettre SIGPIPE à `SIG_DFL` avant tout print
   (crate `libc`, 3 lignes, cfg(unix)) — sinon `rr ... | head` paniquera.

## Bonnes pratiques
- `#![deny(unsafe_code)]` dans rr-core (l'unsafe éventuel vivra dans rr-git).
- Erreurs : `thiserror` dans les libs, `anyhow` uniquement dans rr-cli.
- Toute sortie utilisateur passe par une couche `output.rs` unique dans rr-cli
  (préparera le double contrat texte/JSON de l'issue 07).

## Critères d'acceptation
- [ ] `cargo build --release` vert sur les 3 cibles en CI.
- [ ] `rr version` affiche `rr X.Y.Z (<sha>)`.
- [ ] `rr version | head -0` ne panique pas.
- [ ] clippy pedantic activé sans warning.
