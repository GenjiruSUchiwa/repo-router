---
title: "M3-13 · `rr init` : contrat de navigation + SKILL.md Claude Code"
labels: ["milestone:M3", "type:agent-interface"]
---

## Pourquoi
Leçon n°2 de l'observation : le contrat d'instructions injecté chez l'agent
EST la moitié du produit. Sans lui, l'agent ne sait pas que MAP.md, SYMBOLS.md
et `rr query` existent, et retombe sur grep.

## Quoi
`rr init` écrit : un bloc de navigation dans `CLAUDE.md`/`AGENTS.md`
(créés ou mis à jour entre marqueurs), `.claude/skills/rr-navigation/SKILL.md`,
et un `rr.toml` de config commenté.

## Comment
1. Bloc navigation entre `<!-- rr:begin navigation -->` / `<!-- rr:end -->`
   (idempotent : régénérer remplace le bloc, ne touche à rien d'autre).
   Contenu — la procédure, dans cet ordre :
   1. `rr query "<tâche complète>"` d'abord (zéro appel modèle) ; un
      `FINAL SOURCE ANCHOR` = réponse, copier exactement, s'arrêter ;
   2. sinon greper `.rr/ROUTES.md` (lignes `[ok]`) puis `.rr/SYMBOLS.md` ;
   3. sinon lire MAP.md ; lectures multiples EN PARALLÈLE ;
   4. après résolution manuelle : `rr route add "<tâche>" file#symbol` ;
   5. les cartes routent, la source répond — toujours confirmer dans le code ;
   6. si stale : `rr refresh` puis réessayer une fois.
2. SKILL.md : même procédure au format skill Claude Code (frontmatter
   name/description déclencheurs : « naviguer », « où est », « qui appelle »).
3. `rr.toml` : excludes additionnels, langages, budget MAP — clés réellement
   honorées uniquement (pattern Radar : ne documenter que le vrai).
4. Détection : si `CLAUDE.md` existe → y écrire ; sinon `AGENTS.md` ; sinon
   le créer. `--target <fichier>` pour forcer.

## Bonnes pratiques
- Le texte du contrat vit dans `rr-cli/contracts/*.md` (include_str!), pas
  dans le code — relisible, diffable, traduisible.
- Chaque phrase du contrat doit économiser des tokens à l'agent : relire
  chaque ligne en se demandant « est-ce que ça change son comportement ? ».

## Critères d'acceptation
- [ ] `rr init` deux fois → deuxième run no-op (idempotence).
- [ ] Contenu hors marqueurs jamais modifié (test avec CLAUDE.md existant).
- [ ] Session Claude Code sur le fixture : l'agent utilise `rr query` en
      premier réflexe (test manuel documenté dans la PR).
