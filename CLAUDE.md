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
cargo nextest run --workspace               # tests (cargo test fonctionne aussi)
cargo deny check                            # licences, advisories, sources
cargo +1.95 check --workspace --all-targets # MSRV

# Tests longs / matériel (#[ignore]) — sur la machine de dev :
cargo test --release -p vma-engine --test render_loop -- --ignored --nocapture
VMA_SOAK_SECS=60 cargo test --release -p vma-engine --test render_loop -- --ignored --nocapture

# Soak dans l'application réelle (rapport Markdown, code 2 si images manquées/répétées) :
cargo run --release -- --soak 600 --report tests/soak/reports/$(date +%F)-x.md
cargo run --release -- --soak 600 --hostile --report …   # + agressions fenêtre (checklist docs/manual-tests/)
# Cadence / résolution maître, sources synthétiques GStreamer (cadences réparties en boucle) :
cargo run --release -- --rate 59.94 --resolution 720p --inputs 4 --input-rates 59.94,25,29.97,60
tests/soak/a6-rates.sh                                    # A6 : un soak de 2 min par cadence maître
# Journal des commandes : logs/commands-AAAA-MM-JJ.jsonl par défaut (--journal-dir, --no-journal)

# Regénérer les golden frames après un changement visuel VOULU (puis relire le PNG) :
VMA_UPDATE_GOLDEN=1 cargo test -p vma-compositor --test golden_test_pattern
```
Variables wgpu utiles : `WGPU_BACKEND=vulkan|dx12` ; `WGPU_ADAPTER_NAME=Intel` (tests headless) ; `WGPU_POWER_PREF=low` (app).

## Structure
| Chemin | Rôle |
|---|---|
| `crates/core` | Types communs, sérialisables (`FrameRate` rationnel, `Resolution`, `InputId`, `PixelFormat`, `ColorInfo`, `Rgb8`, `InputKind`, `OutputId`, `VideoCodec`, `Container`). **Ne pas modifier sans le signaler** : tout en dépend. |
| `crates/clock` | Horloge maître à cadence fixe, compteur d'images manquées. |
| `crates/compositor` | wgpu/WGSL : `Mixer` (calques `Rgba16Float`, Cut / Fade gamma ou linéaire, sortie `Rgba8Unorm`), mire, readback (tests). |
| `crates/engine` | `State` + `Command` (thread `vma-control`, journal JSONL), thread de rendu cadencé par l'horloge (plan seqlock, `Sequencer`), triple buffer vers l'UI, stats. |
| `crates/ui` | egui : `Switcher` (Preview / Program, transitions, T-Bar, barre d'inputs, raccourcis, réglages), panneau Statistiques. Tests sans fenêtre avec `egui_kittest`. |
| `app/` | Binaire : assemble fenêtre eframe, moteur, UI. |
| `crates/{io-gst,io-ndi,io-html,audio,replay,api}` | Squelettes vides pour les phases suivantes. |
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
- Parallélisation : une fois les types de `core` figés, un agent par crate dans des git worktrees.
