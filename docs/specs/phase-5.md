# Phase 5 — Contrôle et monitoring : spec d'ensemble et plan

- **Statut** : **proposé** (2026-11-11), à valider par le mainteneur : questions Q1–Q16 du § 7
- **Date** : 2026-11-11
- **Références** : BRIEF §2, §4 (Phase 5), §5, §6, §7, §8 ; ADR 0002 (UI web plus tard, par
  l'API native), ADR 0003 (état, commandes, threads), ADR 0006 (sorties), ADR 0011 (identité
  visuelle) ; [`api-compat.md`](api-compat.md) (fonctions de l'API compat, vérifiées dans la
  documentation publique) ; [`ui-inventaire-reference.md`](ui-inventaire-reference.md)
- **Entrée en phase** : Phase 4 close sous Linux le 2026-11-11 ; soaks Windows en cours chez la
  partenaire (paquet [`packaging-windows.md`](packaging-windows.md)) ; chantier « Interface
  familière » en attente de ses réponses

## 1. Périmètre

### Dans la Phase 5 (BRIEF)
- **Projet** : sauvegarde / chargement d'un fichier versionné (inputs, overlays, sorties,
  réglages audio, transitions…).
- **API de contrôle compatible** (BRIEF §7) : **HTTP** (8088) et **TCP** (8099) de la
  référence, d'après sa documentation publique ; **API native** en parallèle (REST JSON +
  WebSocket d'événements).
- **Tally** exposé par l'API (TCP `TALLY`, abonnements, XML d'état, API native).
- **MultiView** configurable : grille, noms, bordures tally.
- **Acceptation (BRIEF)** : Bitfocus Companion pilote Cut / Fade / overlays par l'API
  compatible **sans modification de son module**.

### Ajouts nécessaires au périmètre
- **Clé stable par input** (identifiant de type GUID) : l'API compat désigne un input par son
  numéro, son nom **ou sa clé**, et le module Companion s'en sert ; elle doit survivre à la
  sauvegarde et au rechargement.
- **Barre de projet** dans l'UI, à l'endroit de la référence (en haut à gauche : Nouveau,
  Ouvrir et récents, Enregistrer, Enregistrer sous, Dernier) ; **sauvegarde automatique** pour
  « Dernier » et la reprise après un plantage.
- **Source d'une sortie** : aujourd'hui toutes les sorties montrent le Program ; la MultiView
  doit pouvoir aller vers le plein écran et le NDI®.
- **Raccourcis configurables** (Q13), par le même vocabulaire que l'API compat
  (`FUNCTION Nom Input=…&Value=…`) : la référence n'en a pas par défaut, les opérateurs
  ont les leurs (`ui-inventaire-reference.md` § 5) ; liste de la partenaire attendue.

### Hors Phase 5 (rappel)
Contrôleur web pour tablette (UI web : son propre ADR, ADR 0002), MIDI / X-Keys / Stream Deck
en direct (Companion les couvre), scripts, sources de données, import des presets de la
référence (format non documenté), enregistrement ISO et replay (Phase 6), PTZ, appels
(Phase 7), fonctions de l'API compat dont VMA n'a pas l'équivalent (titres natifs, PlayList,
calques, MultiCorder…) : refusées avec un message clair (`api-compat.md`).

## 2. Architecture
```
 Companion, Stream Deck, boîtiers tally        outils maison, futur agent IA, UI web
        │ TCP 8099 · HTTP 8088 (compat)               │ HTTP + WebSocket (natif)
        ▼                                             ▼
 ┌──────────── vma-api : runtime tokio à part (2 threads), jamais dans le rendu ───────────┐
 │ compat : FUNCTION → Command (table api-compat.md) ; état → XML ; TALLY / ACTS           │
 │ natif  : POST /commands (Command JSON) ; GET /state (State JSON) ; /events (WebSocket)  │
 └───────────────┬───────────────────────────────────────▲─────────────────────────────────┘
                 │ Command (source = Api), canal borné    │ instantanés d'état (version n),
                 ▼                                        │ événements (tally, sorties)
            vma-control (inchangé : seul à modifier l'état, journal JSONL)
                 │                                        │
                 ▼                                        │
       rendu : Program · Preview · overlays · **MultiView** (passe à part, si une sortie la
       demande) ──► sorties (la source de chacune : Program ou MultiView)
 projet : vma-control ⇄ thread vma-project (lecture / écriture disque, autosauvegarde)
```
- **Une seule porte** : l'API ne touche jamais l'état ; elle envoie des `Command` comme l'UI
  (BRIEF §5), journalisées avec `source: "api"` (déjà prévu par `Origin::Api`). Ce que fait
  Companion se rejoue donc comme ce que fait l'opératrice.
- **Lecture de l'état** : `vma-control` publie un instantané (`Arc<State>` versionné) à chaque
  changement ; l'API le lit sans verrou bloquant et en tire le XML, le JSON et le tally. Un
  client lent ou bloqué ne retient rien : file bornée par connexion, client déconnecté si elle
  déborde.
- **Projet** : un fichier JSON versionné (`format`, `version`), tiré de l'état ; les champs
  d'exécution (état des sorties, signal perdu, compteurs) n'y sont pas. Charger = une suite de
  `Command` ordinaires, avec le même chemin que l'UI (journalisé, rejouable). L'écriture du
  fichier passe par un thread à part (atomique : fichier temporaire puis renommage).
- **MultiView** : une passe du rendu qui compose, à la résolution du maître, les vignettes
  déjà présentes (inputs, Preview, Program) dans une grille, avec bordures tally et noms. Les
  noms sont rastérisés **hors du rendu** quand ils changent (texture par nom) : le rendu ne
  fait que des copies et des rectangles (BRIEF §2.3). Calculée seulement si une sortie la
  montre.

## 3. Critères d'acceptation de la Phase 5
| # | Critère | Comment | Où |
|---|---|---|---|
| E1 | **Aller-retour du projet** : un état complet (tous les types d'inputs, noms, boucle, réglages audio et envois, 4 boutons de transition, Quick Play, lecture auto, domaine de mélange, overlays et leurs transitions, stingers, sorties, bus, casque, FTB) → enregistré → rechargé dans un VMA neuf = même état, champs d'exécution exceptés ; clés d'input conservées | `engine` + `project` | CI |
| E2 | **Fichier abîmé ou d'ailleurs** : JSON invalide, version future, champ inconnu → refus avec la raison, **émission en cours intacte** ; version ancienne → migrée ; fichier média ou caméra absents → input créé en « signal perdu », pas d'échec du chargement ; chemins relatifs au projet retrouvés quand le dossier a bougé | id. | CI |
| E3 | **Charger ne coûte pas d'image** : charger un projet de 10 inputs pendant que le Program tourne → 0 tick manqué ; enregistrer (disque lent simulé) → 0 tick manqué, l'UI ne gèle pas | moteur réel | CI |
| E4 | **Autosauvegarde** : après chaque changement (regroupés, 2 s) et à la fermeture ; fichier jamais à moitié écrit (arrêt brutal simulé) ; « Dernier » rouvre l'état d'avant un plantage ; 5 sauvegardes tournantes | `project` | CI |
| E5 | **API compat HTTP** : `GET /api` = XML d'état bien formé, éléments exigés présents (`api-compat.md` § 3) ; `/api/?Function=…` : 200 si la fonction est faite, 500 sinon ; un test par fonction supportée (requête → effet sur l'état et réponse) | `api` | CI |
| E6 | **API compat TCP** : `FUNCTION`, `XML` (longueur en octets UTF-8), `TALLY`, `ACTS`, `SUBSCRIBE` / `UNSUBSCRIBE` `TALLY` et `ACTS`, `XMLTEXT` (sous-ensemble, Q14), `QUIT`, `PING` ; plusieurs connexions simultanées ; événements dans l'ordre des changements ; fin de ligne `\r\n` ; ligne trop longue ou commande inconnue → `ER`, la connexion reste | `api` | CI |
| E7 | **Tally** : chiffres `0` / `1` / `2` par input dans l'ordre d'affichage, mis à jour au changement suivant (Cut, transition terminée, overlay, Preview) ; événements `ACTS` `Input`, `InputPreview`, `Overlay1–4`, `FadeToBlack`, `Recording`, `Streaming`, `Fullscreen`, son (muet, volume, solo, bus) ; mêmes changements par le WebSocket natif | `api` | CI |
| E8 | **Trafic du module Companion rejoué** : trois connexions comme le module (fonctions, activateurs, XML), `PING` toutes les 3 s, `SUBSCRIBE ACTS`, `XML` toutes les 250 ms ; les commandes exactes qu'il envoie (casse comprise : `fadeToBlack`, `quickPlay`) font leur effet ; son analyseur XML (éléments obligatoires) accepte chaque document | `api` | CI |
| E9 | **Acceptation BRIEF** : Companion réel (version épinglée), module de la référence non modifié, sur la machine de dev : Cut, Fade, Transition 1–4, Preview, Program, Quick Play, overlays 1–4 (In / Out / Off / tout couper), FTB, enregistrement et stream démarrés / arrêtés ; boutons de Companion colorés par le tally ; 20 min de pilotage sans erreur | checklist `phase5-companion.md` | manuel |
| E10 | **API native** : `GET /state` (JSON de l'état), `POST /commands` (une `Command` en JSON, réponse = résultat du moteur), `GET /events` (WebSocket : version de l'état, tally, sorties, journal) ; schéma documenté ; erreurs typées | `api` | CI |
| E11 | **L'API ne touche jamais le Program** : 50 clients, 1 000 commandes / s, clients lents qui ne lisent plus → 0 tick manqué, mémoire bornée, clients lents déconnectés ; API arrêtée ou port pris → VMA démarre quand même, message dans l'UI | moteur réel | CI + soak |
| E12 | **MultiView** : dispositions (Q10) ; golden frames (grille, bordure rouge au Program, verte en Preview, nom) ; bordures au tick suivant le changement ; coût GPU mesuré ≤ 2 ms à 1080p, 10 cases (machine de dev) ; vers le plein écran et le NDI® ; Program inchangé (golden des Phases 1–4) | `compositor` + moteur | CI + machine de dev |
| E13 | **Soak d'acceptation** (20 min, BRIEF §8 ; 1 h avant une version) : 6 inputs, 4 overlays, enregistrement + stream, MultiView en plein écran, un client de l'API qui envoie une commande toutes les 100 ms et lit le XML toutes les 250 ms → 0 image manquée, 0 répétée | soak `--api-load` | machine de dev |
| E14 | **Raccourcis** (si Q13 oui) : touche → fonction (vocabulaire compat), enregistrés avec le projet ou globaux ; la liste de la partenaire reproduite | `ui` | CI |

## 4. Plan (une étape = un commit testé, comme les phases précédentes)
| Étape | Contenu | Critères |
|---|---|---|
| 1 | ADR 0012 (API : tokio / axum, threads, adresse d'écoute, identifiant de protocole), 0013 (fichier projet), 0014 (MultiView, rendu des noms) ; **validation** | — |
| 2 | `core` / `engine` : clé stable par input ; instantané d'état publié par `vma-control` | E1 (clés) |
| 3 | Projet : modèle versionné, état ⇄ projet, chargement par `Command`, thread `vma-project`, autosauvegarde, secrets à part (Q6) | E1–E4 |
| 4 | UI : barre de projet en haut à gauche, récents, confirmation (Q7), « Dernier » | E1–E4 (UI) |
| 5 | `api` : runtime, API native (REST + WebSocket) | E10, E11 |
| 6 | `api::compat` : analyse des fonctions (nom insensible à la casse, `Input` = numéro / nom / clé, `Mix=0`), table des fonctions → `Command` ; XML d'état | E5 |
| 7 | `api::compat` : serveur TCP, tally, activateurs, abonnements | E6, E7 |
| 8 | Trafic du module rejoué en CI ; checklist Companion ; essai sur la machine de dev | E8, E9 |
| 9 | `compositor` : passe MultiView, noms ; `engine` : source d'une sortie | E12 |
| 10 | UI : réglages de la MultiView, source des sorties (Sorties) | E12 (UI) |
| 11 | (Q13) Raccourcis configurables, d'après la liste de la partenaire | E14 |
| 12 | Soak d'acceptation, checklists, **arrêt pour revue** | E13 |

Le projet (étapes 2–4) passe en premier : il sert à chaque émission, et l'API et la MultiView
s'appuient sur la clé d'input et l'instantané d'état.

## 5. Dépendances prévues (licences à confirmer dans les ADR)
`tokio` (MIT, déjà dans l'arbre) ; `axum` (MIT) pour HTTP et WebSocket ; `quick-xml` (MIT,
déjà dans l'arbre) pour écrire le XML ; `uuid` (MIT ou Apache-2.0, déjà dans l'arbre) pour
les clés ; rastérisation des noms de la MultiView : à choisir dans l'ADR 0014 (police déjà
embarquée par egui, ou `ab_glyph` Apache-2.0).

## 6. Risques
- **Le XML attendu par le module va au-delà de la doc publique** : son exemple date d'une
  ancienne version et n'a ni `edition`, ni `audio`, ni `fullscreen`, que le module lit et
  sans lesquels il ignore tout le document. On les connaît par le code du module (MIT,
  public) ; leur forme exacte reste à confirmer (Q3).
- **Le module évolue** : épingler sa version dans la checklist, rejouer son trafic en CI,
  relire ses notes de version avant chaque version de VMA.
- **Ouvrir un port** : l'API compat n'a pas d'authentification (la référence n'en documente
  pas) ; ouverte au réseau, n'importe quel poste peut piloter l'émission (Q4).
- **MultiView et budget GPU** : une passe de plus à chaque tick ; mesurée (E12), calculée
  seulement si une sortie la montre.

## 7. Questions au mainteneur (recommandation en premier)
- **Q1 — Le nom de la référence dans le protocole.** Le module Companion exige que l'élément
  racine du XML d'état porte le nom de la référence : sans lui, il rejette chaque document.
  C'est contraire à la lettre de BRIEF §2.2, mais l'acceptation de la phase (BRIEF §4, §7) en
  dépend. **Recommandé** : l'accepter comme **identifiant de protocole**, en une constante
  unique dans `api::compat`, jamais montrée dans l'UI, ni dans les messages, ni dans la doc
  utilisateur (on y dit « API compat ») ; l'écrire dans l'ADR 0012. Sinon : pas de Companion
  sans modification de son module.
- **Q2 — Version annoncée** (`<version>`, `<edition>`). Le module active des fonctions selon
  le numéro de version (au-delà de 27 : correction colorimétrique, position des calques, que
  VMA n'a pas). **Recommandé** : annoncer une version 26 et l'édition la plus simple qui
  convient (à fixer avec Q3), constantes documentées dans `api-compat.md`.
- **Q3 — Observer le XML réel.** Demander à la partenaire d'ouvrir `http://127.0.0.1:8088/api`
  dans un navigateur, sur sa configuration, et d'enregistrer la page (c'est la sortie publique
  de l'API, sur ses données : comportement observé, BRIEF §2.1). **Recommandé** : oui, avec
  une ligne dans sa checklist ; on ne recopie pas son contenu, on vérifie la forme des
  éléments.
- **Q4 — Où écoutent les API.** **Recommandé** : actives par défaut, sur **127.0.0.1**
  seulement (Companion sur la même machine) ; une case « accessible depuis le réseau » dans
  Réglages, avec un avertissement (pas de mot de passe dans le protocole) ; API native : même
  règle, jeton facultatif plus tard.
- **Q5 — Ports.** **Recommandé** : 8088 (HTTP compat) et 8099 (TCP compat), comme la
  référence, réglables ; API native sur **8090**, réglable.
- **Q6 — Clés de stream et projet.** Les clés ne sont jamais écrites (`Secret`).
  **Recommandé** : ne pas les mettre dans le projet (un projet se partage) ; les garder dans
  un fichier à part du dossier de configuration de l'utilisateur (droits restreints), par nom
  de destination ; au chargement sans clé connue, la destination est créée sans clé et la
  fenêtre Sorties le signale.
- **Q7 — Ouvrir un projet pendant l'émission.** **Recommandé** : confirmation qui liste les
  sorties actives ; elles sont arrêtées proprement avant le chargement ; par l'API, refusé si
  une sortie est active (erreur explicite).
- **Q8 — Autosauvegarde.** **Recommandé** : 2 s après le dernier changement et à la fermeture,
  5 sauvegardes tournantes dans le dossier de configuration ; « Dernier » rouvre la plus
  récente.
- **Q9 — Où va la MultiView.** **Recommandé** : comme source du plein écran (donc **deux
  plein écran** possibles, un par écran : Program sur l'un, MultiView sur l'autre) et du
  NDI® ; un aperçu dans la fenêtre Sorties.
- **Q10 — Dispositions.** **Recommandé** : un jeu fixe (Preview + Program en grand et 8 cases
  dessous ; Preview + Program et 4 ; 3 × 3 ; 4 × 4), chaque case réglable (quel input, ou
  Preview / Program) ; un éditeur libre plus tard si la partenaire le demande.
- **Q11 — Tally d'un overlay.** Un input visible dans un overlay du Program est-il « au
  Program » (rouge) ? **Recommandé** : oui (convention de régie), à confirmer par
  l'observation (Q3, et `TALLY` de la référence).
- **Q12 — Comment prouver l'acceptation.** **Recommandé** : Companion réel sur la machine de
  dev (checklist, version épinglée) **et** son trafic rejoué en CI (E8) ; Companion sans
  interface dans un conteneur en CI plus tard, si le rejeu ne suffit pas.
- **Q13 — Raccourcis configurables dans la Phase 5.** **Recommandé** : oui, avec le
  vocabulaire de l'API compat (une seule table de fonctions), à faire quand la liste de la
  partenaire est arrivée ; sinon reportés.
- **Q14 — `XMLTEXT`.** **Recommandé** : un sous-ensemble de XPath (chemins
  `racine/…/élément[n]/@attribut` et texte d'élément), le reste → `ER` ; c'est ce que les
  boîtiers simples utilisent.
- **Q15 — Format du projet.** **Recommandé** : JSON (BRIEF §6), extension `.vma`, champ
  `version` entier, migrations écrites et testées pour chaque version.
- **Q16 — Chemins des médias.** **Recommandé** : chaque chemin enregistré en absolu **et**
  relatif au projet ; au chargement, le relatif d'abord (dossier déplacé, autre machine),
  puis l'absolu.
