# Phase 2 — Sorties : spec d'ensemble et plan

- **Statut** : **validé** le 2026-10-29 (mainteneur : « oui pour tout », Q1–Q6 selon les
  recommandations du § 7)
- **Date** : 2026-10-29
- **Références** : BRIEF §4 (Phase 2), §5, §8 ; ADR 0001 (PTS, perte du GPU, encodeur de
  secours), ADR 0004 (+ amendement 1, accepté), ADR 0005 ; **ADR 0006 (sorties), accepté**
- **Entrée en phase** : Phase 1 close le 2026-10-29 sous Linux ; soaks Windows (A1, A13)
  **reportés sans échéance** (le mainteneur ne peut pas tester sous Windows), voir `ROADMAP.md`

## 1. Périmètre

### Dans la Phase 2 (BRIEF)
- **Enregistrement local** : H.264 / HEVC via NVENC (repli matériel puis logiciel, ADR 0006
  § 6), **MKV par défaut** (résiste aux coupures), MP4 en option.
- **Streaming RTMP(S)** vers **1 à 3 destinations** simultanées, démarrables ensemble ou une
  par une.
- **Sortie SRT** (caller / listener).
- **Sortie plein écran** sur un écran secondaire (Program).

### Ajouts tirés des phases précédentes
- **FTB** (fondu au noir) : reporté de la Phase 1 (`phase-1.md` § 7.1), il agit sur les sorties
  (enregistrement, streams, SRT, plein écran) et pas sur la vue Program de l'UI.
- **Perte du GPU avec sorties actives** : point différé de l'ADR 0001 (décision Q4).
- **Encodeur de secours en CI** : `openh264enc` (accord ADR 0001), formalisé dans l'ADR 0006.
- **Amendement 1 de l'ADR 0004** (upload hors de la `Queue` partagée) : prérequis, l'étage de
  sortie ajoutant une relecture par tick sur cette même `Queue` (décision Q1).
- Statistiques par sortie dans le panneau et les rapports de soak (BRIEF §8 : « images perdues
  par sortie, file d'attente des encodeurs »).

### Hors Phase 2 (rappel)
Mélangeur audio, audio des inputs, NDI, SRT en entrée (Phase 3) : les sorties portent une piste
**générée** (silence, ou bip de synchro) — ADR 0006 § 5. Overlays (Phase 4). Multiview,
sauvegarde de projet, API (Phase 5) : les réglages de sortie ne sont pas sauvegardés dans un
projet (décision Q3). ISO record (Phase 6). Zero-copy vers l'encodeur (plus tard).

## 2. Architecture (détail dans l'ADR 0006)
```
 thread vma-render (horloge maître), à chaque tick si une sortie est active :
   Program16 ─► FTB ─► out16 ─┬─► conversion Y'CbCr 709 limitée 4:2:0 (NV12)
                              │      └─► copie vers l'anneau de 4 tampons MAP_READ, map_async
                              └─► Rgba8Unorm, triple buffer ─► fenêtre plein écran (thread UI)
                                       │ tampon mappé (n), canal borné, try_send
                                       ▼
 thread vma-output : 1 copie → gst::Buffer (pool), unmap ; audio généré pour n ;
                     même gst::Buffer poussé dans l'appsrc de chaque sortie (file bornée, leaky)
                                       │
       ┌───────────────────────────────┼───────────────────────────────┐
  enregistrement               stream 1..3 (RTMP/S)                SRT
  enc ─► mkv/mp4 ─► fichier    enc ─► flv ─► rtmp2sink             enc ─► mpegts ─► srtsink
  (un pipeline par sortie, créé / arrêté / reconnecté par vma-io, piloté par vma-control)
```
Règles temps réel conservées : le rendu n'alloue pas (hors interne wgpu, dette notée), ne
bloque jamais sur une sortie (tampon indisponible → image non relue et comptée), ne fait pas
d'I/O. Une sortie qui tombe ne fait tomber ni le Program ni les autres sorties.

## 3. Critères d'acceptation de la Phase 2
« Fait » = test vert en CI, ou rapport de soak / checklist manuelle commité si le matériel ou
le réseau est requis (et listé dans `ROADMAP.md`).

| # | Critère | Test | Où |
|---|---|---|---|
| B1 | **Stream RTMP vers un MediaMTX local pendant 1 h, sans déconnexion, dérive A/V ≤ 1 image** (BRIEF) | soak `vma --soak 3600 --stream rtmp://localhost/… --sync-test` + analyseur flash / bip sur le flux relu depuis MediaMTX ; rapport | manuel (machine de dev, Linux) |
| B2 | Budget BRIEF §8, partie Phase 2 : **1 h, 6 inputs 1080p50, 1 stream + 1 enregistrement, 0 image manquée au rendu, 0 image perdue par sortie** (overlays : Phase 4) | soak + rapport | manuel |
| B3 | PTS vidéo et audio = f(*n*) **aux 6 cadences maîtres** : fichier relu, PTS de l'image *k* = *k* × den / num (à la granularité du conteneur), échantillons audio cumulés exacts, trou d'une période pour une image manquée simulée | `engine` + `io-gst`, vidéo brute en MKV (indépendant de l'encodeur) | CI |
| B4 | **MKV lisible après arrêt brutal** (processus tué pendant l'enregistrement) : décodable jusqu'à ≤ 2 s avant l'arrêt | `io-gst`, sous-processus tué | CI |
| B5 | MP4 (option) : valide après arrêt normal ; fragmenté, lisible jusqu'au dernier fragment après arrêt brutal | `io-gst` | CI |
| B6 | **Sortie lente ou bloquée** → images perdues côté sortie, comptées ; **0 image manquée au rendu** ; les autres sorties intactes | `engine` (consommateur factice qui bloque) + `io-gst` (sink qui bloque) | CI |
| B7 | RTMP : 3 destinations simultanées ; serveur coupé → reconnexion auto (1 / 2 / 5 / 10 s), l'enregistrement continue sans trou | `io-gst` avec MediaMTX (CI Ubuntu) ; manuel avec un vrai service si possible | CI + manuel |
| B8 | SRT caller et listener : flux reçu et décodé par un `srtsrc` local ; passphrase ; reconnexion du caller | `io-gst` | CI (si `srtsink` présent sur les runners, sinon manuel, raison notée) |
| B9 | Encodeur : échelle de repli (ADR 0006 § 6), choix et raison affichés ; HEVC sans matériel → refus explicite, rien ne tombe | `io-gst` (éléments masqués) ; NVENC sur la machine de dev | CI + manuel |
| B10 | Conversion de sortie Y'CbCr 709 **plage limitée** : barres de couleur à ±1 code, colorimétrie signalée (caps, en-tête du conteneur) | golden `compositor` + relecture `io-gst` | CI |
| B11 | **FTB** : fondu au noir des sorties de la durée réglée, retour par une seconde commande ; vue Program de l'UI inchangée ; golden à 0 / 50 / 100 % | `engine` + `compositor` | CI |
| B12 | Plein écran sur un écran secondaire : Program (après FTB), 0 image manquée au rendu, l'UI reste utilisable ; checklist (Linux ; Windows ⏸) | checklist `docs/manual-tests/phase2-outputs.md` | manuel |
| B13 | Toute action sur les sorties passe par `Command` et est journalisée ; **la clé de stream et la passphrase SRT n'apparaissent jamais** dans le journal, les logs ni les rapports | `engine` + `ui` | CI |
| B14 | Perte du GPU avec sorties actives : comportement retenu en Q4 : noir aux PTS de *n*, puis finalisation propre | `engine` (`Device::destroy`, comme en Phase 0) | CI |
| B15 | Stats par sortie (état, encodeur, débit, images envoyées / perdues, file, reconnexions, durée, taille du fichier) dans le panneau et les rapports de soak | `ui` sans fenêtre + soak | CI + manuel |
| B16 | CI verte Ubuntu + Windows | workflow | CI |

Si Q1 est validée, les critères de l'amendement 1 de l'ADR 0004 (tick complet p99 ≤ 1 ms avec
4 × 1080p50, `upload_busy` = 0, etc.) font partie de l'étape 1.

## 4. Changements dans `core` (à signaler, BRIEF §10)
Ajouts uniquement, aucun type existant modifié :
- `OutputId` (comme `InputId`) ;
- `VideoCodec` (`H264`, `Hevc`), `Container` (`Mkv`, `Mp4`) : sérialisables, utilisés par
  l'état, le journal, `io-gst` et l'UI ;
- rien d'autre : les réglages complets des sorties (`OutputSpec`) vivent dans `engine`, comme
  `InputSpec`.

## 5. Commandes et état (complète `commands.md`)
```rust
// Ajouts à Command
AddOutput { spec: OutputSpec, name: Option<String> },  // Record | Rtmp { url, key: Secret } | Srt { … } | Fullscreen
UpdateOutput { output: OutputId, spec: OutputSpec },   // refusé si la sortie est active
RemoveOutput { output: OutputId },                     // l'arrête d'abord
StartOutput { output: OutputId }, StopOutput { output: OutputId },
StartRecording, StopRecording,                         // toutes les sorties « enregistrement »
StartStreaming { slot: Option<u8> }, StopStreaming { slot: Option<u8> }, // 1–3, None = toutes
FadeToBlack,                                           // bascule : noir ↔ Program, durée réglée
SetFadeToBlackDuration { ms: u32 },
```
- `Secret` : chaîne dont `Serialize` écrit `"***"` (journal, état diffusé, logs) ; la valeur
  n'est lisible que par `vma-io` au moment d'ouvrir le pipeline. Le rejeu d'un journal qui
  contient un stream demande donc la clé à nouveau (refus explicite sinon).
- État par sortie : `Idle | Starting | Running | Reconnecting { attempt } | Stopping |
  Failed { reason }`, encodeur retenu, et pour l'état global `ftb: { active, progress }`.
- Les compteurs (images envoyées / perdues, débit, file) vivent dans les stats (atomiques), pas
  dans l'état versionné, comme ceux des inputs.

## 6. Réglages par défaut proposés (modifiables dans l'UI)
| Sortie | Défaut |
|---|---|
| Enregistrement | H.264, MKV, débit variable ≈ 40 Mb/s en 1080p50 (CQP/VBR selon l'encodeur), GOP 1 s, dossier `~/Vidéos` (Windows : dossier Vidéos), nom `VMA AAAA-MM-JJ HH-MM-SS.mkv` |
| Stream RTMP | H.264, débit constant 6 Mb/s en 1080p50 (4,5 Mb/s en 1080p30), GOP 2 s, pas d'images B, AAC 160 kb/s |
| SRT | H.264 CBR 8 Mb/s, MPEG-TS, mode caller, latence 120 ms |
| Audio | AAC-LC 48 kHz stéréo, silence (Phase 2) |
| FTB | durée 1 000 ms (provisoire, à observer sur la référence) |
| Plein écran | Program, écran choisi dans une liste (voir limite Wayland § 8) |

La résolution et la cadence des sorties sont celles du maître (pas de mise à l'échelle par
sortie en Phase 2).

## 7. Questions au mainteneur
**Tranchées le 2026-10-29** : les six recommandations sont retenues.

| # | Question | Recommandation |
|---|---|---|
| Q1 | Valider l'**amendement 1 de l'ADR 0004** et l'implémenter en étape 1 ? | **Oui** : la relecture des sorties ajoute un client de la `Queue` ; mieux vaut supprimer la contention d'abord et mesurer l'étage de sortie sur une base saine |
| Q2 | Audio des sorties avant la Phase 3 : **piste AAC générée** (silence, bip de synchro pour les tests) ? | **Oui** : permet de tenir le critère A/V du BRIEF dès la Phase 2 et satisfait les plateformes qui exigent une piste audio ; l'audio réel arrive en Phase 3 sans changer le transport |
| Q3 | Réglages de sortie (destinations, clés) : **non sauvegardés** en Phase 2 (saisis dans l'UI, ou `--record` / `--stream` / `--srt` en ligne de commande), sauvegarde avec le projet en Phase 5 ? | **Oui**, pour ne pas créer de format de fichier avant celui du projet (sinon : un ADR de format dès maintenant, et les clés à protéger sur disque) |
| Q4 | Perte du GPU avec sorties actives : **noir aux PTS de *n*, puis finalisation propre et sortie en erreur** (délai réglable, 10 s), recréation à chaud plus tard ? | **Oui** : la recréation à chaud du device partagé avec eframe est un chantier à part (ADR dédié) ; ceci garantit au moins des fichiers sains et une fin de stream propre |
| Q5 | Un **pipeline et un encodage par sortie** (pas de `tee` partagé) ? | **Oui** (isolation, réglages par destination) ; NVENC grand public suffit pour 1 enregistrement + 3 streams |
| Q6 | Sortie plein écran : **Program seulement** en Phase 2 (Preview, input ou multiview plus tard) ? | **Oui** |

### À observer sur la référence (le partenaire)
Checklist proposée : [`docs/manual-tests/observation-reference-phase2.md`](../manual-tests/observation-reference-phase2.md).
En attendant, les valeurs du § 6 sont utilisées.

## 8. Risques et limites connus
- **Wayland** : un client ne peut pas choisir l'écran d'une fenêtre plein écran ; sous
  Hyprland, la fenêtre passe en plein écran sur l'écran où elle se trouve (à y déplacer à la
  main ou par une règle du compositeur). Documenté dans la checklist.
- **Plein écran et rafraîchissement** : un écran à 60 Hz affichant un maître 50p saccade
  (une image répétée toutes les 5) ; régler l'écran à la cadence maître si possible. La
  présentation de deux surfaces sur le thread UI peut réduire sa fréquence : à mesurer (B12),
  le Program n'en dépend pas.
- **Windows** : aucun test manuel possible pour l'instant (B1, B2, B12 sous Windows ⏸) ;
  couverture par la CI uniquement (WARP, `openh264enc`).
- **Encodeurs absents en CI** : si `openh264enc` ou `srtsink` manquent sur un runner, le test
  concerné est marqué avec sa raison (règle BRIEF §2.5), les tests de PTS (B3) n'en dépendent pas.
- **Charge** : 4 encodages NVENC en 1080p50 + relecture : à mesurer en B2 ; le budget du rendu
  est suivi par l'histogramme par phase.

## 9. Plan d'implémentation (un commit = une fonctionnalité testée)
1. **Amendement 1 de l'ADR 0004** (si Q1) : tampons de transfert mappés, copie enregistrée par
   le rendu, `upload_busy` ; ses critères (3 soaks de 2 min, golden, A12, stress).
2. **ADR 0006 validé** ; `core` : `OutputId`, `VideoCodec`, `Container`. `compositor` : passe FTB,
   conversion vers NV12 (Y'CbCr 709 limitée), mode synchro (flash) ; golden B10, B11.
3. `engine` : anneau de relecture `MAP_READ`, `map_async` sans blocage, thread `vma-output`,
   audio généré verrouillé sur *n*, consommateurs factices ; tests B3 (côté moteur), B6, compteurs.
4. `io-gst` : pipelines de sortie (`appsrc` vidéo + audio → encodeur → mux → sink), échelle
   d'encodeurs, enregistrement MKV / MP4, arrêt propre / erreur disque ; B3 (fichier relu),
   B4, B5, B9 ; CI : `openh264enc` et `gst-libav` installés.
5. `engine` + `ui` : commandes et état des sorties, `Secret`, journal ; panneau Sorties
   (enregistrement, 3 streams, SRT), boutons Enregistrer / Streamer / FTB, stats par sortie ;
   B11 (moteur), B13, B15 (UI).
   *Jalon visible : enregistrer le Program dans un fichier lisible.*
6. Streaming RTMP(S) 1–3 destinations, reconnexion ; MediaMTX en CI ; B7.
7. SRT caller / listener ; B8.
8. Sortie plein écran (viewport eframe sur le même device, triple buffer dédié) ; checklist
   `phase2-outputs.md` ; B12.
9. Perte du GPU avec sorties actives (selon Q4) ; B14.
10. Analyseur de synchro (flux relu depuis MediaMTX : détection du flash et du bip, décalage
    dans le temps), soaks B1 (1 h) et B2 (1 h), rapports, `ROADMAP.md`, arrêt pour revue.

Parallélisation possible après l'étape 2 (types de `core` figés) : `engine` (3) et `io-gst` (4)
en worktrees séparés, l'interface entre eux étant le `gst::Buffer` + PTS de *n*.
