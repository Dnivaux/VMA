# ADR 0013 — Fichier projet

- **Statut** : **accepté** le 2026-11-11 (réponses du mainteneur aux questions Q6, Q7, Q8,
  Q15, Q16 de [`phase-5.md`](../specs/phase-5.md))
- **Date** : 2026-11-11
- **Contexte** : BRIEF §4 (Phase 5 : « fichier JSON ou TOML versionné »), §6 (JSON pour le
  projet) ; ADR 0003 (état et commandes), ADR 0006 (`Secret`)

## Décision
1. **JSON**, extension **`.vma`**, en tête `"format": "vma-project"` et `"version"` (entier,
   1 au départ). Chaque nouvelle version vient avec sa migration depuis la précédente, testée ;
   une version plus récente que celle de VMA est refusée avec la raison.
2. **Contenu** : ce que l'opératrice règle, pas ce qui tourne. Format maître ; inputs (type et
   source, nom, **clé**, boucle, réglages audio et envois, réglages propres à leur type) dans
   l'ordre ; boutons de transition, Quick Play, lecture auto, domaine de mélange ; overlays
   (transitions, dernier input de chaque channel) ; stingers ; sorties (réglages, sans leur
   état) ; bus, casque ; durée du FTB ; plus tard MultiView et raccourcis. Pas dedans : état
   des sorties, signal perdu, compteurs, positions de lecture.
3. **Charger = des `Command`** : le chargeur construit la suite de commandes qui mène d'un état
   vide à l'état du fichier (`AddInput`, `SetInputKey`, réglages…) et la passe par le canal
   ordinaire : même validation, journal, rejeu. Une commande refusée est rapportée (input
   par input) sans arrêter le chargement.
4. **Média absent** : l'input est créé quand même et passe en « signal perdu » (règle
   BRIEF §2.4) ; le chargement ne s'arrête pas.
5. **Chemins** : chaque chemin est enregistré en absolu et relatif au fichier ; au chargement,
   le relatif d'abord, puis l'absolu.
6. **Clés de stream** : jamais dans le projet. Elles vont dans `secrets.json`, dans le dossier
   de configuration de l'utilisateur (droits du seul utilisateur sous Unix), indexées par
   l'adresse de la destination ; une destination chargée retrouve sa clé sur la même machine,
   arrive sans clé ailleurs (signalé dans Sorties). Pas de trousseau du système : absent ou
   verrouillé sur certains postes, il ferait échouer un chargement.
7. **Écriture** sur un thread à part (`vma-project`) : fichier temporaire, `fsync`,
   renommage ; jamais de fichier à moitié écrit, jamais d'I/O sur `vma-control` ni le rendu.
8. **Autosauvegarde** : 2 s après le dernier changement et à la fermeture, 5 fichiers
   tournants dans le dossier de configuration ; « Dernier » rouvre le plus récent.
9. **Ouvrir pendant l'émission** : depuis l'UI, confirmation qui liste les sorties actives,
   arrêtées proprement avant le chargement ; depuis l'API, refusé tant qu'une sortie est
   active.

## Alternatives écartées
- **TOML** : peu adapté aux listes d'objets imbriqués (inputs, sorties) ; JSON est déjà le
  format du journal.
- **Sérialiser `State` tel quel** : il porte l'état d'exécution et ses compteurs internes ; un
  changement de `State` casserait les fichiers. Le projet a son propre modèle, versionné.
- **Remplacer l'état d'un bloc** : contournerait la validation des commandes et le journal.

## Conséquences
- Une nouvelle commande `SetInputKey` (clé d'un input, refusée si elle est déjà prise), et une
  graine des clés journalisée au démarrage (`SeedInputKeys`) pour que le rejeu d'un journal
  redonne les mêmes clés.
