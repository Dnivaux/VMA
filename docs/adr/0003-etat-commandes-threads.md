# ADR 0003 — État, commandes et threads (Phase 1)

- **Statut** : proposé (à valider)
- **Date** : 2026-10-26
- **Contexte** : BRIEF §5 (couche de commandes, state store), §2.3 (temps réel) ; spec
  [`docs/specs/commands.md`](../specs/commands.md)

## Problème
L'UI (puis l'API et une IA) modifie l'état du mélangeur ; le thread de rendu doit en voir
l'effet au tick suivant **sans allocation, sans verrou, sans attente**. Or appliquer une
commande alloue (nouvel input, textures, pipeline, journal, snapshot pour l'UI).

## Décision

### Trois rôles, trois threads
| Thread | Possède | Fait |
|---|---|---|
| UI / API (producteurs) | rien de l'état | envoient des `Command` (`try_send` sur un canal crossbeam **borné**, 1024) ; lisent des snapshots |
| `vma-control` | l'**état** (`State`, versionné) et le cycle de vie des inputs | applique les commandes dans l'ordre, crée / détruit ressources et pipelines, publie snapshot + plan, journalise |
| `vma-render` | rien d'autre que ses ressources attachées | lit le plan à chaque tick, compose, renvoie un retour d'information |

### Ce qui circule
- **Snapshot pour les lecteurs** : `ArcSwap<State>` (arc-swap), remplacé par `vma-control` à
  chaque version. L'UI le charge une fois par frame. Le thread de rendu ne le lit **jamais**.
- **Plan de rendu** : `RenderPlan`, petite structure `Copy` (≤ 64 octets : version, Program,
  Preview, transition en cours), publiée par un **seqlock d'atomiques** (écrivain unique,
  lecteur sans attente qui relit si la séquence a changé). Ni allocation ni `unsafe`.
- **Ressources des inputs** : un tableau fixe de `MAX_INPUTS` (64) emplacements, côté rendu.
  `vma-control` attache un `Arc<InputResources>` à un emplacement via un canal borné ; au
  retrait, le rendu **renvoie** l'`Arc` par un second canal borné pour que la libération ait
  lieu sur `vma-control`, jamais sur le thread de rendu. (Les canaux crossbeam bornés sont
  préalloués : envoyer ou recevoir n'alloue pas.)
- **Retour du rendu** : atomiques `RenderFeedback` (dernier tick, transition terminée n°…),
  lus par `vma-control` toutes les 5 ms.

### Transitions
`vma-control` décide (*quoi*), le rendu horodate (*quand*) :
- **Cut** : nouveau plan (Program = Preview, et inversement) → effectif au tick qui suit sa
  publication.
- **Fade** : le plan porte `Transition { id, from, to, frames, domain }` ; le rendu fixe
  `start` au premier tick où il voit cet `id`, mixe pendant `frames` ticks, puis affiche `to`
  seul et le signale. Tant que `vma-control` n'a pas publié l'état final (swap), le rendu
  continue d'afficher `to` : pas de saut d'une image à la fin du Fade.

### Journal
Chaque commande (acceptée ou refusée) est écrite en JSON Lines par `vma-control`, via un
thread d'écriture (canal borné, pas d'I/O sur `vma-control` non plus) : heure UTC, index
d'image courant, source (`ui`, `api`, `script`), commande, résultat, version d'état.

## Conséquences
- La latence d'une commande = traitement sur `vma-control` (µs) + attente du tick suivant :
  conforme à « Cut effectif au tick suivant » tant que `vma-control` ne bloque pas. Il ne doit
  donc jamais faire d'I/O synchrone ni attendre GStreamer (les changements d'état des
  pipelines sont asynchrones, leurs résultats reviennent comme événements).
- L'état n'a qu'un écrivain : pas de verrou, ordre des commandes déterministe, rejouable depuis
  le journal (utile au débogage et, plus tard, à l'IA de régie).
- `engine` gagne les dépendances déjà déclarées `crossbeam-channel` et `arc-swap`, plus `serde`
  / `serde_json` (BRIEF §6) pour le journal et l'état.

## Alternatives écartées
- **Commandes appliquées sur le thread de rendu** : simple, mais allocations et création de
  ressources dans le chemin chaud.
- **`Mutex<State>` partagé** : verrou dans le chemin chaud, inversion de priorité possible.
- **`ArcSwap<RenderPlan>` lu par le rendu** : le dernier `Arc` pourrait être libéré sur le
  thread de rendu ; le seqlock de valeurs `Copy` l'évite par construction.
