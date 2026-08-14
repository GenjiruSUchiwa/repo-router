---
title: "M1-05 · Fingerprints lexicaux : normalisation des tokens"
labels: ["milestone:M1", "type:core"]
---

## Pourquoi
Le pont entre le vocabulaire humain (« token verification ») et les
identifiants (`verify_token`). La qualité de cette normalisation borne la
recall de tout le ranking lexical.

## Quoi
`rr-core::lex` : fonction pure `terms(&SymbolRecord) -> SmallVec<TermId>`
et son pendant requête `query_terms(&str) -> Vec<TermId>`.

## Comment
1. Splitters : camelCase, PascalCase, snake_case, kebab-case, composantes
   de chemin, chiffres collés (`utf8Decode` → `utf8`, `decode`). Lowercase.
2. Sources des termes d'un symbole, **pondérées par champ** (le poids vit
   dans l'issue 08, ici on tague juste la provenance) : nom, nom qualifié,
   chemin, identifiants de signature, identifiants de corps, appelés.
3. Stemming : conservateur et anglais uniquement — suffixe `s`, `ing`, `ion`
   → forme courte SEULEMENT si la forme courte existe déjà dans le corpus
   (`verification` → `verify` via table de paires courantes ; ne jamais
   stemmer un identifiant de code). En cas de doute : ne pas stemmer.
4. Interning : table globale `term → TermId (u32)` sérialisée dans le
   snapshot ; partout ailleurs on manipule des u32.
5. Stop-words de requête : `where`, `is`, `the`, `how`, `does`, `handled`…
   (liste courte codée en dur, ~40 mots).

## Pseudo-code
```rust
fn split(ident: &str) -> impl Iterator<Item=&str> {
    // "JWTValidator" -> ["jwt", "validator"] ; "verify_token" -> ["verify","token"]
    boundaries(ident).map(|s| s.to_lowercase())
}
```

## Bonnes pratiques
- 100 % de fonctions pures ⇒ tests table-driven exhaustifs
  (`assert_eq!(split("XMLHttpRequest2"), ["xml","http","request","2"])`).
- Documenter chaque règle avec l'exemple qui l'a motivée.

## Critères d'acceptation
- [ ] Table de 30 cas de split passe (dont acronymes collés, digits, unicode).
- [ ] `query_terms("where is token verification handled?")` = `[token, verification, verify]` (ordre stable).
- [ ] Aucune allocation String dans le chemin chaud (vérifier au bench).
