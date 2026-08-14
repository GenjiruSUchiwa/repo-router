# Radar v0.5.0 — Observations empiriques du binaire Linux x86_64

> Addendum à `RADAR_REIMPLEMENTATION_SPEC.md`. Comportements observés en exécutant
> le binaire officiel (checksum vérifié `73f8faa5…`) sur un dépôt fixture
> Rust + Python (8 fichiers, relations appelant/appelé, test). Date : 2026-08-14.
> Licence de la release : MIT (LICENSE.txt inclus dans l'archive).

---

## 1. Le produit réel diffère de l'image donnée par le site

Le `--help` se décrit comme *"the repository cartographer for AI agents"* qui
*"compiles a repository into tiny committed MAP.md routers so agents navigate by
map instead of grep"*. Le produit n'est pas seulement un moteur de requête :
c'est un système de **cartes committées lisibles** (`MAP.md`) plus un cache
local jetable (`.radar/`), plus un **contrat de navigation injecté dans les
instructions de l'agent**.

## 2. Surface CLI observée — 17 sous-commandes

| commande | rôle observé |
|---|---|
| `scan` | walk + hash + extraction, stats (`files: 8  source: 7  defs: 16  refs: 23`, 5 ms) |
| `map` | construit/reconstruit l'arbre MAP.md (`1 written, 0 unchanged, 1 purpose slot(s) pending`) |
| `check` | valide invariants/staleness/budgets, **codes de sortie documentés 0/1/2/3** |
| `tree` | topologie des cartes (alias `ls`) |
| `refresh` | rafraîchissement incrémental « git-gated » |
| `status` | une ligne : état git, cartes stale, slots en attente, routes cachées |
| `impact` | appelants/dépendances/tests déterministes sur un change-set Git |
| `watch` | boucle de refresh par polling |
| `browse` | TUI lecture seule |
| `slots` / `fill` | slots sémantiques à remplir (texte libre validé, ex. « purpose ») |
| `init` | écrit le contrat de navigation (README/AGENTS.md/CLAUDE.md), un `SKILL.md` Claude Code, `radar.toml` |
| `export` | base de connaissances → une page HTML autonome |
| `serve` | `--mcp` = outils stdio pour agents ; sinon vue web localhost GET-only |
| `route` | cache de navigation résolue : `add` / `find` / `list` |
| `query` | résout une tâche localement, zéro appel modèle |
| `agent` | refresh + vérif contrat, puis lance l'agent de code pointé sur `./MAP.md` |

## 3. Artefacts générés

### À la racine (committé)
`MAP.md` (~200 tokens annoncés) avec frontmatter YAML :

```yaml
type: Code Repository Map
map: 1
scope: .
fidelity: syntax
api_hash: e6ffbcaf6a43183c
tokens: ~200
stamped: 2026-08-14T07:25:19Z
```

Contenu : un slot `purpose` (`<!-- radar:slot purpose max=160 -->`, pré-rempli
par une heuristique, destiné à être complété), la section **API** (signatures
publiques par fichier), la section **Tests**.

### Dans `.radar/` (entièrement gitignoré via `.gitignore` contenant `*`)
- `ROUTES.md` — cache requête→anchor **en texte**, auto-amorcé :
  `[auto] verify token | src/auth/token.rs#verify_token | MAP.md | e6ffbcaf | 0 | 2`
  États observés/documentés : `[auto]`, `[ok]`, `[stale]`. Les agents peuvent
  enregistrer leurs découvertes : `radar route add "<tâche>" file#symbol`.
- `SYMBOLS.md` — index trié des symboles publics **en texte greppable** :
  `verify_token → MAP.md · src/auth/token.rs#9`
- `query.bin`, `source.bin`, `state.bin`, `lock` — snapshots binaires (2,5 Ko /
  588 o / 2,2 Ko sur le fixture).

**Insight majeur : les index agent-facing sont des fichiers texte pensés pour
être lus/greppés par un agent en une lecture, pas des API.** Les `.bin` ne
servent qu'au binaire lui-même.

## 4. Contrat de sortie de `query` (observé)

- **Pas de `--json`.** Seules options : `--path`, `--source`. Le contrat est du
  texte stable :

```text
FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
```

- Avec `--source` :

```text
FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
SOURCE SPAN (verified): src/auth/token.rs:9-15
SOURCE COMPLETE
---
<code borné>
```

- Ambiguïté (`"session"`) → liste `source candidates:` (2 anchors), **exit 0**.
- Introuvable → repli `candidate maps:` + frontmatter de la carte, **exit 0**.
- Le contrat d'orientation existe aussi : `FINAL REPOSITORY OVERVIEW`
  (mentionné par le contrat généré par `init`).
- Requête en langage naturel (`"where is token verification handled?"`) →
  routage lexical correct vers `verify_token` sur le fixture.

## 5. Staleness : refus, pas re-parse

Après édition de `src/auth/token.rs` sans réindexation :

```text
STALE SOURCE (no content returned): src/auth/token.rs changed since indexing; run `radar refresh`
```

**Exit 0** malgré le refus. Radar **ne re-parse pas à la volée** (contrairement
au `reparse_and_relocate_symbol` proposé en §27 de la spec) : il refuse et
oriente vers `refresh`. Après `radar refresh` (`0 written, 1 unchanged`), le
span est relocalisé correctement (9-15 → 11-17). Choix plus simple et plus sûr
que la relocalisation à chaud — recommandé pour la réimplémentation.

## 6. `impact` — sortie observée

```text
radar impact - base HEAD (depth 2)
changed (2): MAP.md, src/auth/token.rs
changed definitions (5): …#Claims:5, …#decode_jwt:19, …#verify_token:11, …
direct edges (3):
  src/auth/token.rs -Import line 3-> src/db/users.rs#find_user
  src/auth/token.rs#refresh_token -Call line 30-> src/db/users.rs#find_user
  src/main.rs#main -Call line 6-> src/auth/token.rs#verify_token
callers / affected (1): src/main.rs (distance 1)
dependencies (1): src/db/users.rs (distance 1)
tests (0): none
unresolved references: 12
ambiguous references: 0
```

Arêtes typées avec numéro de ligne, distances, et **compteurs d'irrésolus
affichés honnêtement**. Limite constatée : le test `tests/token_test.rs`
appelant `myapp::auth::token::verify_token` n'a pas été relié (résolution
cross-module incomplète) — cohérent avec le principe §8.3 de la spec (« ne pas
exiger une résolution parfaite »).

## 7. Le contrat de navigation injecté (`radar init`)

`init` écrit dans README.md (ou AGENTS.md/CLAUDE.md) un bloc
`<!-- radar:begin navigation -->` : procédure en 8 étapes pour l'agent
(query d'abord ; sinon greper ROUTES.md puis SYMBOLS.md ; lectures de cartes
**en parallèle** ; « écrire la signature attendue avant de scanner » ; « les
cartes routent, la source répond » ; enregistrer les routes résolues ;
résolution des conflits de merge par régénération). Il embarque aussi la carte
racine directement dans le README (« zero reads ») et crée
`.claude/skills/radar-navigation/SKILL.md` + `radar.toml`.

**C'est la moitié du produit.** La valeur ne vient pas seulement de l'index,
mais du couplage index ↔ instructions de l'agent.

## 8. Divergences spec ↔ réalité (à répercuter)

| point | spec supposait | observé |
|---|---|---|
| sortie machine | `--json` stable partout | pas de `--json` ; contrat texte « copy exactly » + fichiers texte greppables |
| carte committée | `.radar/map` TOML | `MAP.md` à la racine, Markdown + frontmatter YAML, budget tokens, slots |
| `.radar/` | mixte committé/local | 100 % local, gitignoré `*` |
| staleness | re-parse + relocalisation à chaud | refus explicite + `refresh` incrémental |
| codes de sortie | riches par commande | quasi tout à 0 ; seuls `check` (0/1/2/3) discrimine |
| surface | map/query/impact/MCP | 17 commandes, dont route-cache, slots, TUI, export HTML, launcher d'agent |
| cache de routes | absent | `ROUTES.md` auto-amorcé + apprentissage par l'agent (`route add`) |

## 9. Recommandations pour la réimplémentation

1. **Copier le choix « refus si stale »** plutôt que la relocalisation à chaud
   de la spec §27 : plus simple, impossible d'être silencieusement faux.
2. **Produire des index texte greppables** (`SYMBOLS.md`, `ROUTES.md`) comme
   interface agent de premier rang ; garder les `.bin` comme détail interne.
   Ajouter `--json` par-dessus reste une amélioration différenciante possible.
3. **Traiter le contrat d'instructions (`init`) comme un livrable central**,
   pas un bonus : MAP.md embarqué, SKILL.md, procédure de fallback.
4. Le **cache de routes résolues** (auto-amorcé + enrichi par l'agent) est une
   excellente idée absente de la spec — à intégrer en V1.5.
5. Cible différenciante confirmée : **aucun binaire macOS ARM64 n'existe** ;
   le launcher refuse toute plateforme hors linux-x86_64.
6. Détail de robustesse à faire mieux : le binaire panique sur SIGPIPE
   (`radar impact | head` → panic « Broken pipe ») ; gérer proprement EPIPE.

## 10. Chiffres du fixture (indicatifs)

- `scan` : 8 fichiers, 16 defs, 23 refs, 5 ms (stat-cache actif).
- `map` initial : instantané ; artefacts `.radar/` ≈ 6 Ko au total.
- `refresh` après édition d'un fichier : 1 fichier re-parsé, cartes inchangées.
