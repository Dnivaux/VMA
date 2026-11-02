# Phase 3 — Réseau et audio : spec d'ensemble et plan

- **Statut** : **validé** le 2026-11-02 par le mainteneur (« Go pour la suite ») : les
  recommandations des questions Q1–Q9 du § 7 sont retenues
- **Date** : 2026-11-02
- **Références** : BRIEF §2.6, §4 (Phase 3), §5, §6, §8 ; ADR 0003, ADR 0004, ADR 0006 ;
  **ADR 0007 (audio) et ADR 0008 (NDI®), acceptés**
- **Entrée en phase** : Phase 2 close le 2026-11-02 sous Linux ; B12 manuel et soaks Windows
  reportés (voir `ROADMAP.md`)

NDI® est une marque déposée de Vizrt NDI AB.

## 1. Périmètre

### Dans la Phase 3 (BRIEF)
- **NDI en entrée et en sortie**, runtime chargé dynamiquement ; absent → fonctionnalité
  désactivée proprement (ADR 0008).
- **SRT en entrée** (caller / listener, phrase de passe).
- **Mélangeur audio** : gain, mute, solo, vumètres (crête + RMS), décalage par input (ms),
  **bus Master + 2 bus auxiliaires (A, B)**, audio-follow-video optionnel (ADR 0007).
- **Acceptation (BRIEF)** : synchro lèvres vérifiée par un test automatisé (flash + bip
  générés, décalage mesuré inférieur à 1 image).

### Ajouts nécessaires au périmètre
- **Audio des inputs** existants : fichiers vidéo (piste audio), cartes de capture et webcams
  (audio du même pipeline), et nouvel input **périphérique audio** (micro, entrée ligne).
- **Sortie casque** (Q2) : sans elle, le solo n'a pas de sens (la référence fait écouter le
  solo au casque, sans toucher au Master).
- **Choix du bus par sortie** (Q5) : enregistrement, streams, SRT et NDI prennent le Master par
  défaut, ou A / B.
- **Source de synchro** pour les tests : un input synthétique qui produit flash + bip avec
  **sa propre horloge** (cadence, fréquence et dérive en ppm réglables), pour mesurer la synchro
  de bout en bout.
- Fenêtre **« À propos »** (licences, mention de marque NDI®, ADR 0008).

### Hors Phase 3 (rappel)
Effets audio (EQ, compresseur, gate), balance / panoramique, bus C à G, mix-minus, retard de
la vidéo (décalage audio négatif, Q7), sauvegarde des réglages (Phase 5 : projet), audio de
l'input HTML (Phase 4), multiview et API (Phase 5), NDI avec alpha (Phase 4, avec les overlays).

## 2. Architecture (détail dans les ADR 0007 et 0008)
```
 pipelines d'entrée (vma-io) : fichier · capture · périphérique audio · SRT · NDI · synchro
   ├─ vidéo ─► feed (triple buffer, inchangé) ─► rendu : note le temps de flux de l'image affichée
   └─ audio ─► F32 48 kHz stéréo ─► anneau SPSC horodaté (2 s) par input
                                          │
 thread vma-audio, un bloc par tick n :   ▼
   par input : lecture au temps de flux de l'image affichée − décalage, rééchantillonnage
               adaptatif (dérive), gain, mute, audio-follow-video ─► envois Master / A / B
   par bus : fader, mute ; vumètres crête + RMS (inputs et bus, avant / après fader)
        ├─► anneau des blocs de bus (n) ─► vma-output : image n + bloc n du bus de chaque sortie
        └─► mix casque (Master, ou solo) ─► pipeline casque (appsrc ─► sink du périphérique)
 sorties (Phase 2 inchangées) + NDI : appsrc NV12 + audio ─► ndisinkcombiner ─► ndisink
```
Toutes les horloges GStreamer = horloge système monotone (celle du maître). Le rendu n'en fait
pas plus qu'un `try_send` par tick vers `vma-audio`. Une source audio qui tombe donne du
silence ; un thread audio en retard donne du silence compté aux sorties : ni le Program ni la
vidéo des sorties ne l'attendent.

## 3. Critères d'acceptation de la Phase 3
| # | Critère | Comment | Où |
|---|---|---|---|
| C1 | **Synchro lèvres < 1 image** (BRIEF) : source de synchro → Program → enregistrement relu par l'analyseur (Phase 2) : \|bip − flash\| < 1 image, **aux 6 cadences maîtres**, source à une autre cadence que le maître (25 → 50, 29,97 → 59,94, 50 → 25…) | `io-gst` + moteur réel, analyseur | CI |
| C2 | **Décalage par input** : source de synchro dont le son est **en avance** de 80 ms (`audio_lead_ms`) → décalage mesuré 80 ms ; décalage de l'input réglé à 80 ms → < 1 image ; décalage appliqué à ± 1 ms | id. | CI |
| C3 | **Dérive** : source à +100 ppm et −100 ppm (temps simulé : 1 h ; réel : 60 s) → décalage stable < 1 image, aucun trou ni recalage, `r_i` estimé à ± 5 ppm | `audio` (temps simulé) + `io-gst` | CI ; soak 1 h manuel (C11) |
| C4 | **Mélange exact** : gain (dB → linéaire, rampe sans clic), mute, envois Master / A / B, fader et mute des bus, somme de plusieurs inputs ; écrêtage signalé, pas d'enroulement | `audio`, blocs connus | CI |
| C5 | **Solo** : casque = inputs en solo (AFL par défaut, PFL en option), plusieurs solos possibles ; **Master inchangé** à l'échantillon près | `audio` | CI |
| C6 | **Audio-follow-video** : contribution au Master selon l'input au Program ; Cut au tick (rampe d'un bloc) ; Fade : fondu croisé en puissance constante sur la durée de la transition ; bus A / B inchangés | `audio` + `engine` (temps simulé) | CI |
| C7 | **Vumètres** : sinus connu → crête et RMS à ± 0,1 dB, silence → −∞ ; avant / après fader ; publiés sans verrou, affichés (UI sans fenêtre) | `audio` + `ui` | CI |
| C8 | **Temps réel** : thread audio sans allocation ni verrou bloquant (test d'allocation comme le rendu) ; source qui tombe → silence et compteur, reprise sans clic ; thread audio retardé artificiellement → silence compté aux sorties, **0 tick manqué au rendu** | `audio` + `engine` | CI |
| C9 | **SRT en entrée** : caller et listener, phrase de passe, reçu depuis notre sortie SRT (Phase 2) image et son, synchro < 1 image ; émetteur parti → signal perdu, revenu → reconnecté | `io-gst` | CI (si `srtsrc` présent, comme B8) |
| C10 | **NDI absent** : runtime introuvable → inputs et sorties NDI refusés avec raison, UI grisée avec explication, aucun effet sur le reste | `io-ndi` + `ui` | CI (2 OS) |
| C11 | **NDI présent** : boucle locale sortie NDI → input NDI, image et son, synchro < 1 image ; découverte de la source ; émetteur arrêté → signal perdu, relancé → reconnecté | `io-ndi` | conditionnel (`VMA_REQUIRE_NDI`) : machine de dev |
| C12 | **Périphériques audio** : input micro / ligne (identifiant stable, débranché → perdu → rebranché), sortie casque (choix du périphérique, solo audible) | `io-gst` (faux périphérique en CI) + checklist `phase3-audio.md` | CI + manuel |
| C13 | **Commandes et journal** : tout réglage audio, NDI, SRT passe par `Command`, journalisé ; phrase de passe SRT jamais écrite (B13 étendu) ; rejeu du journal → même état audio | `engine` + `ui` | CI |
| C14 | **Soak d'acceptation** : 1 h, 6 inputs 1080p50 **avec audio** (dont 2 à ± 100 ppm), mélangeur actif, 1 stream + 1 enregistrement (+ sortie NDI si runtime) : 0 image manquée, 0 perdue par sortie, 0 trou audio, **synchro < 1 image et dérive < 1 image sur 1 h** (analyseur sur le stream relu) | soak + rapport | manuel (Linux) |
| C15 | CI verte Ubuntu + Windows | workflow | CI |

## 4. Changements dans `core` (à signaler, BRIEF §10)
Ajouts uniquement, aucun type existant modifié dans son sens :
- `InputKind` : variantes `Ndi`, `Srt`, `AudioDevice` (et `has_video()`, faux pour
  `AudioDevice`) ;
- `AudioBus` (`Master`, `A`, `B`) : sérialisable, utilisé par l'état, le journal, les sorties
  et l'UI ;
- rien d'autre : gains, envois, décalages vivent dans l'état de `engine`, comme `InputSpec`.

## 5. Commandes et état (complète `commands.md`)
```rust
// Ajouts à InputSpec
Ndi { source: String },                                        // nom annoncé « MACHINE (Nom) »
Srt { uri: String, mode: SrtMode, latency_ms: u32, passphrase: Option<Secret> },
AudioDevice { device: DeviceId },
SyncSource { rate: FrameRate, sample_rate: u32, drift_ppm: i32, audio_lead_ms: i32 }, // tests

// Ajouts à OutputSpec
Ndi { name: String },                                          // une au plus

// Ajouts à Command
SetInputGain { input, db: f32 }, SetInputMute { input, mute: bool },
SetInputSolo { input, solo: bool }, SetInputDelay { input, ms: u32 },
SetInputBus { input, bus: AudioBus, on: bool }, SetAudioFollowVideo { input, on: bool },
SetBusGain { bus, db: f32 }, SetBusMute { bus, mute: bool },
SetOutputBus { output, bus: AudioBus },                        // refusé si la sortie est active
SetMonitor { device: Option<DeviceId>, solo_mode: SoloMode },  // AFL | PFL
```
- État audio par input : gain, mute, solo, décalage, envois, audio-follow-video ; par bus :
  gain, mute ; sortie casque : périphérique, mode de solo. Versionné et journalisé comme le
  reste ; les vumètres et compteurs sont dans les stats (atomiques), pas dans l'état.
- Valeurs hors bornes (gain > +10 dB, décalage > 1 000 ms…) : refusées avec raison.

## 6. Réglages par défaut proposés (modifiables dans l'UI)
| Réglage | Défaut (à confirmer sur la référence, checklist `observation-reference-phase3.md`) |
|---|---|
| Format interne | 48 kHz, stéréo, F32 |
| Fader input / bus | 0 dB, plage −∞ à +10 dB (loi dB) |
| Envois d'un nouvel input | Master seulement |
| Audio-follow-video | activé pour les inputs qui ont image et son, désactivé pour un périphérique audio |
| Mute d'un nouvel input | non muet si audio-follow-video, sinon muet (pas de son surprise à l'antenne) |
| Solo | AFL (après fader), plusieurs solos permis ; PFL en option |
| Décalage | 0 ms, 0 à 1 000 ms par pas de 1 ms |
| Vumètres | crête (maintien 1,5 s) + RMS (fenêtre 300 ms), échelle −60 à 0 dBFS, rafraîchis à la cadence de l'UI |
| Bus des sorties | Master |
| Casque | périphérique par défaut du système, écoute le Master sauf solo |
| Entrée SRT | caller, latence 120 ms (comme la sortie) |
| Sortie NDI | nom « VMA Program », Program + Master |

## 7. Questions au mainteneur (tranchées le 2026-11-02 : recommandations retenues)
| # | Question | Recommandation |
|---|---|---|
| Q1 | Périphériques audio par **GStreamer** plutôt que `cpal` (écart au BRIEF §6, ADR 0007 § 7) ? | **Oui** : même horodatage que la vidéo, un seul inventaire, pas de dépendance |
| Q2 | **Sortie casque** dans la Phase 3 ? | **Oui** : condition du solo |
| Q3 | NDI par **`gst-plugin-ndi` compilé dans le binaire** (MPL-2.0, exception `cargo deny`), runtime externe (ADR 0008) ? | **Oui** |
| Q4 | **Pas de runtime NDI en CI** (licence à accepter) : chemin « absent » en CI, tests réels sur la machine de dev ? | **Oui** ; il faudra installer le runtime sur la machine de dev (AUR `ndi-sdk`) |
| Q5 | **Choix du bus par sortie** (Master / A / B) ? | **Oui**, défaut Master |
| Q6 | Audio-follow-video : **fondu croisé avec le Fade**, bus A / B non concernés ? | **Oui**, à confirmer sur la référence |
| Q7 | Décalage **positif seulement** (retarder le son, 0–1 000 ms) ; retarder la vidéo plus tard ? | **Oui** |
| Q8 | Réglages audio, NDI, SRT **non sauvegardés** (comme les sorties, projet en Phase 5) ? | **Oui** |
| Q9 | Tout en **stéréo** : mono dupliqué, au-delà de 2 canaux les 2 premiers ? | **Oui** (le multicanal attendra un besoin réel) |

Machine de dev : il faudra installer `gst-plugins-bad` (`srtsrc`, déjà manquant en Phase 2) et
le runtime NDI.

## 8. Risques et limites connus
- **Horloges** : l'hypothèse « horloge système GStreamer = `Instant` » doit être vérifiée sous
  Windows (QPC) ; sinon, une conversion par paire d'instants mesurés ensemble (étape 2).
- **Horodatage des sources réseau** : `ndisrc` et `srtsrc` horodatent à l'arrivée ou par les
  temps de l'émetteur selon leur mode ; la gigue réseau passe dans `image_i(n)` lissé et dans la
  DLL. À mesurer (C9, C11) ; la latence SRT réglée amortit la gigue.
- **Sources dont l'image est en retard sur le son** (caméra USB qui encode) : seul un décalage
  positif est possible en Phase 3 (Q7).
- **Windows** : pas de test manuel (C12 casque et micro, C14 soak ⏸) ; NDI sous Windows :
  ni en CI ni à la main tant que le mainteneur ne peut pas tester sous Windows.
- **Coût** : 6 inputs audio + 3 bus + casque : faible ; la sortie NDI non compressée 1080p50
  (≈ 125 Mb/s) charge le réseau local, pas le rendu (même relecture NV12 que les sorties).
- **Marque NDI** : suivre les règles de marque publiées ; toute question → mainteneur.

## 9. Plan d'implémentation (un commit = une fonctionnalité testée)
1. **ADR 0007 et 0008 validés** ; `core` : `InputKind` (`Ndi`, `Srt`, `AudioDevice`),
   `AudioBus`. Crate `audio` sans GStreamer : anneau horodaté, DLL + rééchantillonneur, mixeur
   (gain, mute, envois, bus, solo AFL / PFL, audio-follow-video), vumètres ; C3 (temps simulé),
   C4, C5, C6 (côté mixeur), C7 (calcul), C8 (allocation).
2. `io-gst` : horloge système monotone pour tous les pipelines (test de concordance avec
   `Instant`, 2 OS) ; branches audio des inputs (fichier, capture) vers les anneaux ; source de
   synchro (`SyncSource`, dérive simulée) ; `engine` : temps de flux de l'image affichée.
3. `engine` : thread `vma-audio` cadencé par les ticks, paramètres par seqlock, blocs de bus
   vers `vma-output` (qui ne génère plus l'audio, sauf `--sync-test`) ; **C1, C2, C3** de bout en
   bout (enregistrement relu par l'analyseur), C6 (moteur), C8 (thread retardé).
   *Jalon visible : le son d'un fichier vidéo dans l'enregistrement, calé sur l'image.*
4. `engine` + `ui` : commandes et état audio, journal ; panneau Mélangeur (une tranche par input
   : fader, mute, solo, M / A / B, décalage, audio-follow-video, vumètres ; tranches des bus),
   bus de chaque sortie ; C7 (UI), C13.
5. Périphériques audio : input micro / ligne, sortie casque (solo audible) ; faux périphérique en
   CI ; checklist `docs/manual-tests/phase3-audio.md` ; C12.
6. Entrée SRT (caller / listener, phrase de passe, reconnexion) ; C9.
7. `io-ndi` : plugin statique, détection du runtime, `IoStatus::ndi`, fenêtre « À propos » ;
   input NDI (découverte, connexion, perte et reconnexion) ; C10, C11 (entrée).
8. Sortie NDI (Program + bus) ; boucle locale sortie → entrée avec synchro ; C11.
9. Soak d'acceptation C14 (1 h), rapport, `ROADMAP.md`, arrêt pour revue.

Parallélisation possible après l'étape 1 (types de `core` et interface de l'anneau figés) :
`audio` (1), `io-gst` (2, 6) et `io-ndi` (7) en worktrees séparés.
