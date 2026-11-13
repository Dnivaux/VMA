# CLAUDE.md — conventions du projet VMA

Résumé opérationnel de `BRIEF.md` (qui reste la référence). À lire au début de chaque session.

## Le projet en une phrase
Mélangeur vidéo live open source en Rust (switcher, streaming, enregistrement), multiplateforme
(Linux + Windows, macOS plus tard). Nom provisoire : **VMA** (ancien nom de code « Aiguillage », encore utilisé dans `BRIEF.md`).

## Règles non négociables (BRIEF §2)
1. **Clean room** : uniquement docs publiques, standards ouverts, comportement observé. Jamais de
   décompilation, jamais de copie de code / assets / textes d'un produit tiers.
2. **Aucune marque tierce** dans le code, l'UI, les binaires. On dit « API compat » (`api::compat`).
3. **Le temps réel prime** : dans le thread de rendu, pas d'allocation applicative, pas de lock
   bloquant, pas d'I/O synchrone (les logs passent par `tracing-appender` non bloquant).
4. **Pas de `unwrap()` / `expect()` / `panic!`** hors tests (imposé par les lints clippy du
   workspace). Erreurs typées `thiserror` ; `anyhow` seulement dans `app/`. Une entrée qui tombe
   passe en « signal perdu », elle ne fait jamais tomber le programme.
5. **Honnêteté** : `ROADMAP.md` sans pourcentage. « Fait » = tests d'acceptation verts en CI.
   Tout test désactivé ou `#[ignore]` est listé avec sa raison.
6. **Licences** : vérifier et noter la licence de chaque dépendance (commentaire dans
   `Cargo.toml` racine). GPL interdite dans le binaire (contrôlé par `cargo deny`). SDK NDI chargé
   dynamiquement, jamais embarqué.

## Commandes
```sh
cargo run                                   # lance l'application (fenêtre + Program)
cargo fmt --all                             # formatage
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace               # tests (préférer nextest : un processus par test ; sous
                                            # `cargo test` les tests de temps réel d'io-gst se gênent,
                                            # sauf avec `-- --test-threads=1`, ROADMAP Phase 6)
cargo deny check                            # licences, advisories, sources
cargo +1.95 check --workspace --all-targets # MSRV

# Tests longs / matériel (#[ignore]) — sur la machine de dev :
cargo test --release -p vma-engine --test render_loop -- --ignored --nocapture
VMA_SOAK_SECS=60 cargo test --release -p vma-engine --test render_loop -- --ignored --nocapture
cargo test --release -p vma-engine --test outputs -- --ignored --nocapture   # coût de l'étage de sortie 1080p50

# Soak dans l'application réelle (rapport Markdown, code 2 si images manquées/répétées) :
cargo run --release -- --soak 600 --report tests/soak/reports/$(date +%F)-x.md
cargo run --release -- --soak 600 --hostile --report …   # + agressions fenêtre (checklist docs/manual-tests/)
# Cadence / résolution maître, sources synthétiques GStreamer (cadences réparties en boucle) :
cargo run --release -- --rate 59.94 --resolution 720p --inputs 4 --input-rates 59.94,25,29.97,60
tests/soak/a6-rates.sh                                    # A6 : un soak de 2 min par cadence maître
# Journal des commandes : logs/commands-AAAA-MM-JJ.jsonl par défaut (--journal-dir, --no-journal)
# Projet (ADR 0013) : ouvert au démarrage, à son format ; autosauvegarde dans ~/.config/vma/autosave
# (coupée pendant un soak et par --no-autosave) :
cargo run --release -- --project emission.vma
# Enregistrer le Program dès le démarrage (sinon bouton « Enregistrer », dossier Vidéos) :
cargo run --release -- --record /tmp/vma-rec --soak 10 --report /tmp/vma-rec/soak.md
# Program en plein écran dès le démarrage (nom de l'écran, ou numéro : voir « screens=[…] » au démarrage) :
cargo run --release -- --fullscreen 2 --soak 600 --report …
# Stream RTMP dès le démarrage (clé : --stream-key ou VMA_STREAM_KEY, masquée dans le rapport),
# mode synchro (flash + bip par seconde) et relecture du flux pendant le soak (B1) :
VMA_STREAM_KEY=k cargo run --release -- --stream rtmp://127.0.0.1/live --sync-test --soak 60 --sync-check rtmp://127.0.0.1/live/k
VMA_MEDIAMTX=…/mediamtx tests/soak/phase2-b1-b2.sh       # B1 + B2 : 1 h, 6 inputs, enregistrement + stream
VMA_MEDIAMTX=…/mediamtx tests/soak/phase3-c14.sh         # C14 : 1 h, 6 sources synchro avec son (±100 ppm), mélangeur, enr. + stream + NDI
VMA_MEDIAMTX=…/mediamtx tests/soak/phase5-e13.sh         # E13 : 20 min, D11 + MultiView en plein écran + client de l'API compat (--api-load)
# GPU perdu : noir sur les sorties pendant N s (défaut 10), puis finalisation et sortie en erreur :
cargo run --release -- --record /tmp/vma-rec --gpu-loss-grace 5

# Input HTML (ADR 0009) : vma-html, compilé à part (télécharge CEF ; cache local conseillé),
# placé à côté de vma dans target/<profil>/ ; puis la page en input au démarrage :
CEF_PATH=~/.cache/vma-cef cargo build --release -p vma-io-html --features cef --bin vma-html
cargo run --release -- --html page.html                   # ou http(s)://…, file://…
target/release/vma-html --vma-probe --vma-seconds=20      # essai CEF (étape 4), rapport sur la sortie
VMA_HTML=$PWD/target/release/vma-html VMA_REQUIRE_HTML=1 cargo test --release -p vma-io-html --test real_helper
VMA_HTML=$PWD/target/release/vma-html VMA_REQUIRE_HTML=1 cargo test --release -p vma-io-gst --test html

# Regénérer les golden frames après un changement visuel VOULU (puis relire le PNG) :
VMA_UPDATE_GOLDEN=1 cargo test -p vma-compositor --test golden_test_pattern
```
Variables wgpu utiles : `WGPU_BACKEND=vulkan|dx12` ; `WGPU_ADAPTER_NAME=Intel` (tests headless) ; `WGPU_POWER_PREF=low` (app).

## Structure
| Chemin | Rôle |
|---|---|
| `crates/core` | Types communs, sérialisables (`FrameRate` rationnel, `Resolution`, `InputId`, `PixelFormat`, `ColorInfo`, `Rgb8`, `InputKind`, `OutputId`, `VideoCodec`, `Container`, `AudioBus`, `AlphaMode`, `OverlayChannel`). **Ne pas modifier sans le signaler** : tout en dépend. |
| `crates/clock` | Horloge maître à cadence fixe, compteur d'images manquées. |
| `crates/compositor` | wgpu/WGSL : `Mixer` (calques `Rgba16Float` à alpha prémultiplié, Cut / Fade gamma ou linéaire, stinger et overlays 1–4 incrustés par `Keys`, sortie `Rgba8Unorm`), `OutputStage` (FTB, flash de synchro, NV12 BT.709 limitée pour les sorties), mire, readback (tests). |
| `crates/engine` | `State` + `Command` (thread `vma-control`, journal JSONL), thread de rendu cadencé par l'horloge (plan seqlock, `Sequencer`), triple buffer vers l'UI, stats ; sorties : `OutputSpec` / `Secret` (jamais écrit : `***`), état et runs des sorties, FTB, anneau de relecture NV12, thread `vma-output` (PTS et audio dérivés de *n*, `OutputHub` / `OutputConsumer`, un thread par sortie). |
| `crates/ui` | egui : `Switcher` (Preview / Program, transitions, T-Bar, FTB, barre d'inputs, raccourcis, réglages, boutons Enregistrer / Streamer, fenêtre Sorties), fenêtre plein écran (`fullscreen` : viewport différé, `Screens`), panneau Statistiques (dont `output_stats`). Tests sans fenêtre avec `egui_kittest`. |
| `app/` | Binaire : assemble fenêtre eframe (sur sa propre boucle winit, pour lister les écrans : `screens.rs`), moteur, UI. |
| `crates/io-gst` | `vma-io` : pipelines d'entrée (capture, fichiers, images fixes), et de sortie (`GstOutput` + `OutputFeed` : échelle d'encodeurs, MKV / MP4), démarrées / arrêtées sur requête de `vma-control` (`outputs`). |
| `crates/audio` | Audio temps réel sans GStreamer (ADR 0007) : anneau horodaté par input, DLL (fréquence de la source), lecture au temps de flux rééchantillonnée, `Mixer` (faders, mutes, envois Master / A / B, solo AFL / PFL au casque, audio-follow-video), vumètres. |
| `crates/io-ndi` | NDI® (ADR 0008) : plugin `gst-plugin-ndi` enregistré statiquement, détection du runtime (`status()`), jamais embarqué. |
| `crates/io-html` | Input HTML (ADR 0009, `docs/specs/io-html.md`) : `HtmlHost` (lance et surveille `vma-html`, TCP local + jeton, images par mémoire partagée), sans CEF ; binaire `vma-html` (feature `cef`, hors des builds ordinaires : mode service et sonde `--vma-probe`). |
| `crates/project` | Fichier projet (ADR 0013) : `ProjectSnapshot` de `vma-engine` en JSON versionné (`.vma`), chemins des médias relatifs et absolus, clés de stream à part (`secrets.json`), écriture atomique, autosauvegarde (thread `vma-project`, 5 fichiers tournants). |
| `crates/api` | API de contrôle (ADR 0012) sur un runtime tokio à part : native (`api-native.md` : `POST /commands`, `GET /state`, `/tally`, WebSocket `/events`, 127.0.0.1:8090) ; compat à venir (étapes 6–7). Tests contre `vma_engine::headless` (moteur sans rendu). |
| `crates/replay` | Replay (Phase 6, ADR 0015–0016), sans GStreamer : `segments` (enregistrement par input sur disque, images et son : segments de 2 s + index horodaté sur l'horloge partagée, lisible pendant l'écriture, effacement des plus anciens), `player` (`Playhead` : image montrée à chaque tick, en images de la caméra, vitesse, arrière, pas à pas, angle). L'enregistreur (`io-gst::record`) et le lecteur (`io-gst::replay_player`) sont dans `io-gst` ; l'état et les commandes dans `engine::replay`. |
| `docs/adr/` | Décisions d'architecture (une par fichier, à faire valider). 0001 archi (acceptée avec amendements), 0002 décisions produit. |
| `docs/specs/` | Une spec par module, avec critères d'acceptation. |
| `docs/manual-tests/` | Checklists de tests sur matériel réel. |
| `tests/golden/` | Images de référence. `tests/soak/reports/` : rapports de soak. |

## Conventions
- Identifiants de code en anglais ; docs et textes d'UI en français.
- Commits : anglais, Conventional Commits (`feat(clock): …`), un commit = une fonctionnalité testée.
- `unsafe` interdit par défaut (`unsafe_code = "deny"`) ; s'il devient nécessaire (NDI, interop),
  l'autoriser localement avec un commentaire `// SAFETY:` justifié.
- Les tests d'intégration autorisent explicitement `unwrap`/`panic` en tête de fichier.
- Couleur (ADR 0005) : valeurs R'G'B' Rec.709 encodées, pleine plage, partout ; calques et
  Program internes `Rgba16Float` (`vma_compositor::Mixer`), sortie UI `Rgba8Unorm` arrondie dans
  le shader. Transitions en domaine gamma par défaut (linéaire en option), linéaire pour le
  scaling / filtrage.
- PTS des sorties = f(index d'image *n*), jamais l'heure murale.
- Triple buffer Program : l'UI n'appelle `ProgramReader::update()` qu'en début de frame, après
  avoir soumis la frame précédente ; une seule `Queue` partagée (invariant, ADR 0001).
- Cadences : tout le code reste rationnel (`FrameRate`) ; multi-cadence maître dès la Phase 1.

## Méthode (BRIEF §10)
- Avant une phase : spec dans `docs/specs/` + plan, **attendre la validation**.
- Toute décision structurante (dépendance lourde, archi, format de fichier) → ADR à valider.
- Une ambiguïté de comportement → poser la question au mainteneur plutôt que deviner.
- Pas de fonctionnalité hors de la phase courante, pas de refactor massif non demandé.
- Soaks d'acceptation : **20 min** par phase ; le soak d'**1 h** (budget BRIEF §8) seulement avant
  une version, ou quand une phase touche l'horloge, l'audio ou les sorties (décision du 2026-11-09).
- Parallélisation : une fois les types de `core` figés, un agent par crate dans des git worktrees.
