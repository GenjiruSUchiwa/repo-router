# #14 — re-ancrage sur l'arbre courant, et cinq amendements de plus

Le plan de #14 a été écrit contre `6bff9b0`. Depuis, `docs/` a été supprimé (`697478e`,
déplacé vers le wiki), #12/#13/#40/#43 ont fusionné, et deux commits ont retiré tous les
commentaires inline — donc **aucun** numéro de ligne du plan n'est valide. La règle du plan
tient : « le chemin et le nom de l'item sont le contrat, l'entier une commodité ». Ce fichier
ne renumérote pas ; il liste ce qui a *bougé de sens*, disparu, ou déjà été livré.

Tout ci-dessous a été relu dans l'arbre à `3180072`.

## 1. Ancres vérifiées — inchangées de sens

`index/mod.rs` `Resolution::Resolved` / `unresolved_count` / `ImportRecord::owner` ·
`index/build.rs` `ReferenceKind::MethodCall` · `facts.rs` `resolves_by_path` / `Import::name` ·
`main.rs` `finish` · `refresh.rs` `value_parser` · `result.rs` `exit_code` ·
`ranking.rs` `DEFAULT_RANKING_PROFILE` / `CANDIDATE_LIMIT` / `RANKING_PROFILE_VERSION` /
`cap_cut_a_tie` · `repo/state.rs` `observe_state` · `content.rs` `acquire_for_source` ·
`map.rs` `collected_lang` · `verify.rs` `MAX_SOURCE_BYTES` · `workspace.rs` `LOCAL_DIR` ·
`snapshot.rs` `SNAPSHOT_MAGIC` · `text/mod.rs` `is_reserved_artifact_name` / `SYMBOLS_PATH` /
`IGNORE_PATH` · `text/validate.rs` `is_publishable` / `ConflictReason::as_str` /
les onze variantes de `ConflictReason` · `LoadOutcome` · `RebuildReason` · `SnapshotLabel`.

D1, D2, D3, D4, D5, D15, D16 sont intacts. `crates/rr-core/benches/` existe déjà avec six
benches `harness = false` : §9.1 suit le motif, elle ne l'invente pas.

## 2. Ancres déplacées ou disparues

| Le plan dit | L'arbre dit |
|---|---|
| `docs/query.schema.json` | `crates/rr-cli/tests/query.schema.json`, `$id` épinglé sur ce chemin, `title: "RepoRouterQueryResultV1"` |
| `V1_SCHEMA_SHA256 = 726b57e2…` | **`2dc6a9daa904176c4e443fba167af509c3ece7d62992f095915f1ff7b912fcf8`** — à recalculer dans la branche avant de figer |
| `docs/SPEC.md` §16.2 / §73, `docs/ranking-self-review.md` | supprimés (wiki). Voir amendement H |
| `refresh/report.rs` `REPORT_SCHEMA_VERSION = 3` | `REFRESH_SCHEMA_VERSION = 4` **et** `STATUS_SCHEMA_VERSION = 3` ; voir amendement I |
| `text/model.rs:250`, « la dernière promesse `#NN` » | déchargée par #12. La dernière est `text/validate.rs` (`issue #14 will branch on them`), que §6.3.1 décharge |
| `FACT_SCHEMA_VERSION = 4`, `SNAPSHOT_SCHEMA_VERSION = 7`, `BUILD_VERSION = 4` | `9`, `12`, à relire. §3.3 **lit** les constantes, ne les recopie pas |

## 3. Amendement H — la prose sans `docs/` va sur l'item, pas dans un fichier

§7.3 (« `docs/SPEC.md` §16.2 gagne la promesse D8 ») et §9.4 (« `docs/SPEC.md:73` gagne une
phrase ») n'ont plus de cible. Elles vont là où le code les rechecke :

- la promesse D8 → doc de module de `json_contract.rs` (amendement I) et doc de `render_json` ;
- la règle « un chiffre de benchmark n'est pas une garantie » → doc de module de `quality.rs`,
  au-dessus de `PerfDecision`, qui l'applique structurellement (aucun champ en ms).

D9 est inchangé : le fichier de revue était la *source*, les trois doc comments
(`DEFAULT_RANKING_PROFILE`, `CANDIDATE_LIMIT`, `rank`) sont le livrable, et ils survivent.

D10 est **déjà déchargé** : `issues/14-impact-check-corpus.md:38-41` porte la phrase
« directional, not our unrelated-workload guarantee ». §9.4 perd sa première moitié.

## 4. Amendement I — D7 n'est plus une décision de #14, c'est une règle publiée

#43 a livré `crates/rr-core/src/json_contract.rs` : un module normatif, sans déclaration,
qui dit que toute nouvelle surface publie `schema_version` en première clé, que `rr query`
est la seule exception avec son `v`, et qui tient **la table constante → surface → valeur**.

Conséquences : D7 est confirmé mais cesse d'être un arbitrage ; `IMPACT_SCHEMA_VERSION` et
`CHECK_SCHEMA_VERSION` suivent le nommage `<SURFACE>_SCHEMA_VERSION` ; et #14 a une
obligation nouvelle que le plan ignore — **inscrire les deux constantes dans la table de
`json_contract.rs`**, sinon la règle documente deux surfaces sur quatre.

## 5. Amendement J — l'inventaire « fermé à quatorze » est faux : `SOURCE BYTES` manque

§7.1 déclare l'inventaire des marqueurs clos. Il omet `SOURCE BYTES: {n}`
(`render.rs`, juste au-dessus de `---`). Ce n'est pas un marqueur mineur : `agent.rs` le
publie dans le bloc de contrat comme *« la dernière ligne d'en-tête, immédiatement au-dessus
de `---` »*, et trois suites de tests le parsent par préfixe. Le geler oublié, c'est laisser
hors du gel le seul marqueur qu'un agent lit **positionnellement**.

L'inventaire réel est de seize littéraux : les quatorze listés, plus `SOURCE_BYTES`, plus
`CANDIDATES_HEADER` (`"source candidates:\n"`, minuscule, que la liste nomme déjà).
§7.1 doit aussi dire pourquoi les marqueurs de `agent.rs` sont hors périmètre — ou les inclure.

## 6. Amendement K — §7.2 rétrécit, la moitié existe déjà

`crates/rr-cli/tests/query_contract.rs` livre déjà `published_schema()`, `declared_members()`
et `query_contract_json_carries_exactly_the_members_the_schema_declares` sur
`DirectResult` / `CandidatesResult` / `NoneResult`, plus l'équivalent pour la source vérifiée.
`ANCHOR_MARKER` y est déjà une constante.

Reste à écrire dans `frozen_v1.rs` : le pin SHA-256 du schéma, `Anchor` et `CandidateItem`
(non couverts), le littéral `v == 1` sur ses trois sites, le test verbatim des seize
marqueurs, et les codes de sortie `0/2/3/4` + `1`. Ne pas dupliquer les helpers : les
réutiliser ou les remonter dans `tests/common/`.

## 7. Amendement L — décision sur D11 : RR03xx part maintenant, RR06xx reste réservé

D11 réservait les deux familles faute de *validateur propriétaire*. Le critère n'a pas
changé ; ce sont les faits qui ont changé, et ils ont changé **différemment** pour les deux.

### RR0301–RR0303 : implémentées dans #14

Les propriétaires existent tous, aucun second parseur à écrire :

| Règle | Propriétaire appelé |
|---|---|
| `RR0301_ROUTES_INVALID` | `text::routes::load_routes` ⇒ `Option<RouteFault>` |
| `RR0302_ROUTE_ANCHOR_MISSING` | `render::decode_anchor` puis `result::resolve_anchor` |
| `RR0303_ROUTE_API_STALE` | `RouteRecord::api_identity` vs `text::catalog::api_identity` |

Mais #12 a livré un modèle **différent** de celui que la table d'origine supposait, donc
trois corrections :

1. **`RR0301` est un `warning`, pas une `error`.** La doc de `RouteFault` dit que toutes les
   variantes mènent à la même action — jeter le fichier et repartir vide — et qu'elle existe
   « pour être *rapportée*, non branchée ». Un cache local reconstructible qui se répare seul
   est la classe de `RR0401_CACHE_CORRUPT`, pas celle d'un artefact commité cassé. Le nom du
   `RouteFault` va dans `actual`, ce qui préserve exactement ce que l'enum fermé achète.
2. **`RR0302` s'applique à chaque enregistrement.** Il n'y a pas de colonne de statut :
   `RouteRecord` porte `key`, `anchor`, `map`, `api_identity`, `confidence`. Le
   `[ok]`/`[stale]`/`auto` de la table d'origine ne décrit aucun format livré — le retirer
   du libellé, sinon la règle cite une grammaire qui n'existe pas.
3. **`RR0303` est un diagnostic unique, pas N.** `api_identity` est l'identité API *du
   corpus*, pas de la portée : elle est identique pour toutes les lignes. Émettre une ligne
   par route ferait passer un cache périmé de 1 024 entrées pour 1 024 défauts.

### RR0601–RR0603 : réservées, avec une raison neuve

#13 a fusionné, mais sa logique vit dans `crates/rr-cli/src/init.rs` — `apply_region`,
`install_skill`, `resolve_existing_name`, privées — et c'est un **chemin d'écriture** :
`text::block::apply_block` calcule et écrit d'un seul geste. Or `check()` vit dans `rr-core`
et ne peut pas appeler `rr-cli` : la dépendance va dans l'autre sens.

Donc les deux seules voies sont (a) extraire un planificateur `init` en lecture seule dans
`rr-core` — un changement à part, qui touche une commande livrée — ou (b) écrire un second
parseur dans `check.rs`, exactement ce que D11 interdit. **Elles restent réservées**, et le
seam `TODO(#13)` change de libellé : plus « en attente de #13 », mais « en attente d'un
planificateur `init` en lecture seule dans `rr-core` ». À inscrire au registre des reports
(#46), qui existe pour ça.

`RR0604` est inchangée et tire déjà sur `ConflictReason::ManagedIgnore` seul.

## 8. Ce que #43 a déjà réglé pour §6

`ConflictReason` dérive maintenant `serde::Serialize` avec `rename_all = "kebab-case"`.
La note de *Dependencies* est satisfaite : `DiagnosticV1.actual` porte ce jeton kebab-case,
`message` porte `as_str()` verbatim, et #14 n'écrit **aucun** second rendu.

## 9. Ordre revu

§10 tient, avec une correction : E et F ne dépendent plus de rien *et* leurs propriétaires
sont tous livrés — ce sont les deux slices à partir en premier. F rétrécit (amendement K),
E grossit un peu (RR03xx, amendement L). A → B → D est inchangé, C en parallèle après A.
Les quatre bloqueurs de la section *Dependencies* sont fermés : #14 et #46 sont les deux
seules issues ouvertes.
