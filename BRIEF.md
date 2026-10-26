# BRIEF — Mélangeur vidéo live open source en Rust

> **Pour Claude Code** : ce document décrit le produit, l'architecture cible et la façon de travailler.
> Lis-le en entier avant toute action. Ne commence pas à coder tout le projet d'un coup : on avance **phase par phase**, en commençant par la Phase 0 (section 11).
> Nom de code provisoire du projet : **Aiguillage**. Tous les identifiants de code sont en anglais ; la documentation peut être en français.

---

## 1. Vision

Construire un **logiciel de production vidéo live** (switcher / mélangeur, streaming, enregistrement), open source, écrit en Rust, fonctionnellement comparable aux logiciels de régie logicielle du marché (référence fonctionnelle : vMix).

Différenciateurs :
- **Multiplateforme** : Linux et Windows en priorité, macOS ensuite (la référence est Windows uniquement).
- **Open source** (licence MIT OU Apache-2.0, au choix de l'utilisateur).
- **Compatible avec l'écosystème existant** via une API de contrôle compatible (Bitfocus Companion, Stream Deck, tally).
- **Pensé pour l'automatisation** : à terme, ce moteur servira de cœur à une régie automatisée par IA (cadrage, bascule caméra, replay, overlays) pour le sport (handball en premier) et le podcast.

Utilisateur / mainteneur : développeur (Rust, Python, Node, Electron/React), expérience terrain en production broadcast sport (captations handball). Machine de dev : RTX 5090 + Intel Core Ultra 9 285K, dual boot Linux (Omarchy / Arch) et Windows.

---

## 2. Règles non négociables

1. **Clean room.** Toute fonctionnalité est implémentée à partir de la documentation publique, de standards ouverts et du comportement observé. Interdit : décompiler, désassembler ou inspecter les binaires d'un produit commercial ; copier du code, des assets, des icônes, des textes ou des images d'un produit tiers.
2. **Aucune marque tierce.** Le nom « vMix », ses logos et sa charte n'apparaissent nulle part dans le code, l'UI ou les binaires. Dans le code, on parle de « compat API » (module `api::compat`), pas de « vMix ». L'interface ne doit pas être une copie pixel près.
3. **Le temps réel prime sur tout.** Une image perdue ou un plantage à l'antenne est le pire bug possible. Aucune allocation, aucun lock bloquant, aucune I/O disque ou réseau synchrone dans le thread de rendu.
4. **Pas de `unwrap()` / `expect()` / `panic!` dans les chemins live** (rendu, I/O, audio). Erreurs typées (`thiserror`), et une entrée qui tombe ne doit jamais faire tomber le programme : elle passe en état « signal perdu » (image noire ou mire) et tente de se reconnecter.
5. **Honnêteté sur l'avancement.** Le fichier `ROADMAP.md` ne contient **jamais** de pourcentage d'avancement estimé. Une fonctionnalité est « faite » uniquement si ses tests d'acceptation passent en CI. Si un test est désactivé, l'écrire explicitement.
6. **Licences.** Avant d'ajouter une dépendance, vérifier sa licence et l'indiquer. GPL interdite dans le binaire principal (on reste compatible MIT/Apache). Le SDK NDI est propriétaire : le charger dynamiquement au runtime (pas de redistribution embarquée), et respecter ses conditions de marque.

---

## 3. Glossaire (vocabulaire du domaine)

| Terme | Sens dans ce projet |
|---|---|
| Input | Source : caméra, carte de capture, fichier, image, NDI, SRT, page HTML, couleur, etc. |
| Preview | L'input préparé, qui passera à l'antenne à la prochaine transition |
| Program | Ce qui est actuellement à l'antenne |
| Transition | Passage Preview → Program : Cut, Fade (mix), Wipe, Stinger |
| Stinger | Transition animée (vidéo avec alpha) qui masque la bascule à un point donné |
| Overlay channel | Couche superposée au Program, indépendante (4 au MVP, 8 à terme) : lower thirds, scores, logos |
| Multiview | Mosaïque de monitoring de toutes les sources + Preview + Program |
| Bus audio | Mélange audio séparé (Master, A, B…) envoyable vers des sorties distinctes |
| Tally | Signal indiquant quelle source est en Preview (vert) / Program (rouge) |
| ISO record | Enregistrement séparé de chaque input, pour la post-production |
| Replay | Ralenti instantané à partir des dernières secondes enregistrées en mémoire |

---

## 4. Périmètre fonctionnel par phase

Chaque phase a des **critères d'acceptation** testables. On ne passe pas à la phase suivante sans que ceux de la phase courante passent.

### Phase 0 — Fondations (voir section 11 pour la tâche détaillée)
- Workspace Cargo, CI (Linux + Windows), lint (`clippy -D warnings`), format, structure des crates.
- Horloge maître + boucle de rendu à cadence fixe (50 fps par défaut).
- Fenêtre `egui` avec un Program qui affiche une mire générée sur le GPU.
- **Acceptation** : la boucle tourne 10 min à 50 fps sans image manquée (compteur exposé), CI verte sur les 2 OS.

### Phase 1 — Mélange de base
- Inputs : webcam / carte de capture (via GStreamer), fichier vidéo (lecture en boucle), image fixe, couleur unie.
- Compositeur GPU : Preview + Program, transitions Cut et Fade (durée réglable).
- UI : liste des inputs, fenêtres Preview et Program, boutons Cut / Fade, raccourcis clavier.
- **Acceptation** : 4 inputs 1080p50 simultanés, 0 image perdue sur 30 min, test golden-frame du Fade à 50 %.

### Phase 2 — Sorties
- Enregistrement local (H.264 / HEVC via NVENC, conteneur MKV par défaut pour résister aux coupures, MP4 en option).
- Streaming RTMP(S) vers 1 à 3 destinations simultanées.
- Sortie SRT (caller / listener).
- Sortie plein écran sur un écran secondaire.
- **Acceptation** : stream RTMP vers un serveur local de test (ex. MediaMTX) pendant 1 h sans déconnexion ni dérive A/V supérieure à 1 image.

### Phase 3 — Réseau et audio
- NDI en entrée et en sortie (SDK chargé dynamiquement ; si absent, la fonctionnalité est désactivée proprement).
- SRT en entrée.
- Mélangeur audio : gain, mute, solo, vumètres (peak + RMS), décalage par input (ms), bus Master + 2 bus auxiliaires, audio-follow-video optionnel.
- **Acceptation** : synchro lèvres vérifiée par un test automatisé (flash + bip générés, décalage mesuré inférieur à 1 image).

### Phase 4 — Habillage
- 4 overlay channels indépendants, avec transition d'entrée/sortie.
- Input HTML : rendu hors écran d'une page web (CEF) avec canal alpha, à 50 fps. Doit pouvoir afficher des overlays HTML existants (lower thirds, scores) et recevoir des données via une API (mise à jour du score sans recharger la page).
- Transition Stinger (vidéo avec alpha, point de bascule configurable).
- **Acceptation** : un lower third HTML animé passe en overlay sans saccade, golden-frame sur l'alpha.

### Phase 5 — Contrôle et monitoring
- Multiview configurable (grille, labels, bordures tally).
- **API de contrôle compatible** (section 7) : HTTP + TCP.
- Tally exposé via l'API.
- Sauvegarde / chargement d'un projet (fichier JSON ou TOML versionné : inputs, overlays, sorties, réglages audio).
- **Acceptation** : Bitfocus Companion pilote Cut / Fade / overlays via l'API compatible sans modification de son module.

### Phase 6 — Sport
- Enregistrement ISO de chaque input.
- Replay : tampon circulaire en mémoire par caméra (durée configurable), marquage d'événements, lecture ralentie (25 %, 50 %, 75 %), lecture multi-angle, envoi à l'antenne.
- **Acceptation** : marquer un événement et le rejouer au ralenti en moins de 3 actions, sans impacter le Program live.

### Phase 7 et au-delà (hors MVP, ne pas implémenter sans demande)
- Invités à distance en WebRTC (équivalent « call »).
- Pilotage de caméras PTZ (VISCA over IP).
- Plugins d'automatisation IA (bascule automatique de caméra, détection d'actions de jeu) : prévoir dès maintenant que **toute action de l'UI passe par une couche de commandes**, pour qu'une IA puisse piloter le mélangeur exactement comme un humain.

---

## 5. Architecture

```
                 ┌───────────────────────── Couche de commandes ─────────────────────────┐
                 │   UI egui   │   API HTTP   │   API TCP   │   (futur) agent IA / scripts │
                 └──────────────────────────────┬────────────────────────────────────────┘
                                                │ Command (enum), via canal (crossbeam)
                                                ▼
                                   ┌─────────────────────────┐
                                   │   Engine / State store  │  état unique, versionné,
                                   │   (source de vérité)    │  diffusé en snapshots
                                   └───────────┬─────────────┘
                                               │
     ┌──────────────── Inputs ───────────────┐ │ ┌──────────── Rendu (thread dédié) ──────────┐
     │ GStreamer pipelines (capture, fichier,│ │ │ Horloge maître 50 fps                     │
     │ NDI, SRT, HTML/CEF)                   │─┼▶│ Compositeur wgpu : Preview, Program,      │
     │ → dernière image dispo par input      │   │ transitions, overlays, multiview          │
     │   (triple buffer, jamais bloquant)    │   └──────────────┬────────────────────────────┘
     └───────────────────────────────────────┘                  │ image Program (+ audio)
                                                                 ▼
     ┌──────────── Audio (thread temps réel) ──┐   ┌──────────── Sorties ────────────────────┐
     │ mixage, bus, vumètres, décalages        │──▶│ GStreamer : NVENC → fichier / RTMP /    │
     └─────────────────────────────────────────┘   │ SRT / NDI ; écran plein écran           │
                                                    └─────────────────────────────────────────┘
```

### Principes
- **Horloge maître** : le thread de rendu cadence tout. À chaque tick, il prend la **dernière image disponible** de chaque input (pas d'attente). Si un input n'a pas de nouvelle image, on réutilise la précédente. Les inputs ont des cadences hétérogènes (25, 30, 50, 59,94 fps) : ils sont rééchantillonnés sur l'horloge maître.
- **Pas de lock dans le chemin chaud** : triple buffers / `arc-swap` / canaux bornés. Si un consommateur (sortie) est en retard, on **lâche une image côté sortie** et on incrémente un compteur ; on ne ralentit jamais le rendu.
- **Couche de commandes** : l'UI ne modifie jamais l'état directement. Elle envoie des `Command` (ex. `Cut`, `Fade { duration_ms }`, `SetPreview { input_id }`, `OverlayIn { channel, input_id }`). Même chemin pour l'API et le futur agent IA. Toutes les commandes sont journalisées (utile pour le debug et pour entraîner plus tard l'IA de régie).
- **Upload GPU** : au MVP, les images arrivent en mémoire système (`appsink`) en NV12 / I420 et sont uploadées dans des textures wgpu ; la conversion YUV → RGB se fait dans un shader. Le zero-copy (interop CUDA / Vulkan / D3D11) est une optimisation **ultérieure**, isolée derrière un trait.
- **Couleur** : travailler en espace linéaire dans le compositeur, Rec.709 en entrée/sortie. Gérer correctement la plage limitée (16–235) vs pleine.

### Découpage du workspace Cargo (proposition, à affiner en Phase 0)

```
aiguillage/
├── Cargo.toml              # workspace
├── BRIEF.md                # ce document
├── CLAUDE.md               # conventions pour Claude Code (à créer en Phase 0)
├── ROADMAP.md              # état réel, sans pourcentages
├── docs/
│   ├── adr/                # décisions d'architecture (1 fichier par décision)
│   └── specs/              # 1 spec par module, avec critères d'acceptation
├── crates/
│   ├── core/               # types communs : Frame, InputId, Rational fps, erreurs
│   ├── clock/              # horloge maître, cadence, métriques d'images perdues
│   ├── engine/             # state store, Command, boucle principale, projet (save/load)
│   ├── compositor/         # wgpu : shaders, transitions, overlays, multiview
│   ├── io-gst/             # inputs et sorties GStreamer
│   ├── io-ndi/             # NDI (feature optionnelle, chargement dynamique)
│   ├── io-html/            # input navigateur (CEF), feature optionnelle
│   ├── audio/              # mixeur, bus, vumètres
│   ├── replay/             # (Phase 6) tampons circulaires, ralenti
│   ├── api/                # HTTP + TCP, module compat
│   └── ui/                 # egui
├── app/                    # binaire principal : assemble tout
└── tests/
    ├── golden/             # images de référence
    └── soak/               # tests longue durée
```

---

## 6. Stack technique

| Besoin | Choix | Remarques |
|---|---|---|
| Langage | Rust stable, édition 2024 | MSRV à fixer en Phase 0 |
| I/O média | `gstreamer-rs` + plugins officiels + `gst-plugins-rs` | NVENC (`nvh264enc`/`nvh265enc`), RTMP (`rtmp2sink`), SRT (`srtsrc`/`srtsink`), NDI (`ndisrc`/`ndisink` de gst-plugins-rs) |
| Rendu / compositing | `wgpu` (WGSL) | Vulkan sous Linux, D3D12 sous Windows |
| UI | `egui` + `eframe` (backend wgpu) | Partage direct des textures GPU : preview et multiview sans copie vers une webview |
| Audio | `cpal` pour les périphériques, mixeur maison | Thread temps réel, pas d'allocation |
| HTML hors écran | CEF (bindings Rust) | Feature Cargo optionnelle, isolée dans `io-html` |
| API | `axum` (HTTP), `tokio` (TCP) | Runtime tokio séparé du thread de rendu |
| Concurrence | `crossbeam-channel`, `arc-swap`, `parking_lot` hors chemin chaud | |
| Erreurs / logs | `thiserror`, `anyhow` (uniquement dans `app/`), `tracing` | |
| Sérialisation | `serde` + JSON (projet), XML pour l'API compat | |
| Tests | `cargo nextest`, `insta` (snapshots), golden frames maison | |

Toute déviation de ce tableau passe par un ADR dans `docs/adr/`, soumis à validation avant implémentation.

---

## 7. API de contrôle compatible

Objectif : que les outils existants (Bitfocus Companion, Stream Deck, boîtiers tally) fonctionnent **sans modification**, en implémentant l'interface publiquement documentée par la référence.

À implémenter dans `crates/api/src/compat/` à partir de la **documentation publique officielle** (section « Developer API » / « Shortcut Function Reference » de la doc en ligne de la référence) :
- **HTTP** (port 8088 par défaut) : appel de fonctions par query string (`/api/?Function=Cut`, `/api/?Function=Fade&Duration=500`, `/api/?Function=PreviewInput&Input=2`…) et état complet en XML sur `/api/`.
- **TCP** (port 8099 par défaut) : commandes texte ligne par ligne (`FUNCTION`, `XML`, `TALLY`, `SUBSCRIBE`…).

Consignes :
- **Vérifier chaque nom de fonction, paramètre et format de réponse dans la documentation officielle** avant implémentation. Ne rien inventer de mémoire. Lister dans `docs/specs/api-compat.md` les fonctions supportées, partiellement supportées et non supportées.
- Commencer par le sous-ensemble utile : `Cut`, `Fade`, `PreviewInput`, `ActiveInput`, `OverlayInput1..4` (In/Out), `StartRecording` / `StopRecording`, `StartStreaming` / `StopStreaming`, réglages audio de base, tally.
- Une **API native** propre (REST JSON + WebSocket d'événements) est exposée en parallèle ; la couche compat est une traduction vers les `Command` internes.
- Tests de conformité : pour chaque fonction supportée, un test qui envoie la requête et vérifie l'effet sur l'état et le format de réponse.

---

## 8. Budgets de performance (cibles mesurées en CI ou en test manuel documenté)

| Métrique | Cible MVP |
|---|---|
| Images perdues au rendu | 0 sur 1 h, 6 inputs 1080p50, 4 overlays, 1 stream + 1 enregistrement |
| Latence entrée → Program (hors capture) | ≤ 2 images |
| Dérive audio / vidéo | < 1 image sur 1 h |
| Temps de bascule Cut | effectif au tick suivant la commande |
| Démarrage de l'application | < 3 s (hors chargement CEF) |
| Utilisation GPU | mesurée et affichée dans l'UI (panneau stats) |

Un panneau « Statistiques » dans l'UI affiche en permanence : fps réel, images perdues (rendu / par sortie), temps de rendu par image, file d'attente des encodeurs.

---

## 9. Stratégie de tests

1. **Tests unitaires** dans chaque crate (`cargo nextest`).
2. **Golden frames** : scènes de test déterministes (mires, couleurs, sources synthétiques) rendues par le compositeur, comparées à des images de référence (PSNR ≥ seuil défini par test). Couvre transitions, overlays, alpha, conversions couleur.
3. **Sources synthétiques** : `videotestsrc` / `audiotestsrc` GStreamer et un générateur « flash + bip » pour mesurer synchro et latence. La CI ne dispose ni de caméras ni de GPU NVIDIA : prévoir un backend logiciel (wgpu peut tourner en logiciel ; encodeur x264 de secours) pour que les tests passent en CI, et marquer les tests nécessitant du matériel `#[ignore]` avec la raison.
4. **Soak tests** : exécutions longues (1 h) lancées manuellement sur la machine de dev ; rapport écrit dans `tests/soak/reports/`.
5. **Conformité API** (section 7).
6. **Tests manuels sur matériel réel** : quand une phase en a besoin, Claude Code rédige une checklist précise dans `docs/manual-tests/` et demande au mainteneur de l'exécuter.

---

## 10. Méthode de travail pour Claude Code

- **Avant de coder une phase** : rédiger la spec du module dans `docs/specs/` (comportement, interfaces publiques, critères d'acceptation) et un plan d'implémentation. Attendre la validation du mainteneur.
- **Petits incréments** : une PR / un commit logique = une fonctionnalité testée. Messages de commit en anglais, format Conventional Commits.
- **Parallélisation** : une fois les interfaces entre crates figées (traits et types dans `core`), les crates peuvent être développées en parallèle dans des git worktrees séparés (un agent par crate). Ne jamais modifier les types de `core` sans le signaler, car tous les autres crates en dépendent.
- **Décisions** : toute décision structurante (nouvelle dépendance lourde, changement d'architecture, format de fichier projet) → un ADR court dans `docs/adr/NNNN-titre.md`, à valider.
- **Questions** : en cas d'ambiguïté sur le comportement attendu d'une fonctionnalité, poser la question plutôt que deviner. Le mainteneur peut observer le comportement de la référence et répondre.
- **Ne pas faire** : pas de fonctionnalités hors de la phase courante, pas de refactor massif non demandé, pas de dépendance GPL, pas d'`unsafe` sans commentaire `// SAFETY:` justifié, pas de pourcentage d'avancement auto-déclaré.
- **`CLAUDE.md`** : à créer en Phase 0, il résume les conventions de ce document (commandes de build/test, structure, règles 2.1 à 2.6) pour les sessions futures.

---

## 11. Première tâche : Phase 0

1. Lire ce document et poser les questions éventuelles avant de commencer.
2. Proposer (et faire valider) : MSRV, licence finale (MIT OR Apache-2.0), structure du workspace, liste des dépendances avec leurs licences.
3. Créer le workspace Cargo avec les crates de la section 5 (vides ou squelettes), `CLAUDE.md`, `ROADMAP.md`, `docs/adr/0001-architecture-generale.md`.
4. Mettre en place la CI GitHub Actions : build + clippy + fmt + tests, sur Ubuntu et Windows. Documenter l'installation de GStreamer pour le dev local (Arch Linux et Windows).
5. Implémenter `clock` : horloge à cadence fixe (fps rationnel, 50/1 par défaut), compteur d'images manquées, métriques via `tracing`.
6. Implémenter un premier `compositor` minimal : une mire générée en shader WGSL, affichée dans une fenêtre `eframe` (Program) avec un panneau stats (fps, images manquées, temps de rendu).
7. Test : la boucle tourne 10 minutes à 50 fps sans image manquée (test `#[ignore]` lancé manuellement), plus un test golden-frame de la mire.
8. Mettre à jour `ROADMAP.md` et s'arrêter pour revue.

---

## 12. Décisions ouvertes (à trancher avec le mainteneur)

- **UI** : `egui` retenu par défaut pour la performance (textures GPU partagées). Alternative envisagée : Tauri + React, pour réutiliser l'écosystème d'un projet existant du mainteneur (app Electron/React d'overlays broadcast). Compromis possible : moteur + preview en egui, et une UI web de contrôle à distance (tablette) en React par-dessus l'API native.
- **Nom définitif** du projet (« Aiguillage » est provisoire).
- **Format des titres** : HTML via CEF uniquement, ou également un moteur de titres natif (texte + formes en wgpu) plus léger.
- **Cadences supportées au MVP** : 50p seul, ou aussi 25p / 59,94p / 60p dès la Phase 1.
- **macOS** : quand l'ajouter à la CI.
