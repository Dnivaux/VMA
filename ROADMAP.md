# ROADMAP — état réel

Règles (BRIEF §2.5) : pas de pourcentage. Une fonctionnalité est **faite** uniquement si ses
tests d'acceptation passent en CI. Les tests désactivés ou manuels sont listés explicitement.

Légende : ✅ fait (tests verts) · 🟡 implémenté, validation incomplète · ⏸ reporté (décision du mainteneur, raison notée) · ⬜ pas commencé

---

## Phase 0 — Fondations · *terminée, avec un test reporté (soak hostile Windows)*

ADR 0001 **accepté avec amendements** ; décisions produit dans l'ADR 0002 (nom VMA, egui,
titres HTML + natifs, multi-cadence dès la Phase 1, macOS plus tard).

| Élément | État | Preuve |
|---|---|---|
| Workspace Cargo (12 crates `vma-*`, édition 2024, MSRV 1.95) | ✅ | CI `msrv` |
| Lints : clippy `-D warnings`, `unwrap`/`expect`/`panic` interdits hors tests | ✅ | CI `test` (Ubuntu, Windows) |
| Format (`rustfmt`) | ✅ | CI `fmt` |
| Politique de licences (`cargo deny`) | ✅ | CI `deny` (3 advisories « unmaintained » et les polices d'egui en exception justifiée dans `deny.toml`) |
| CI GitHub Actions (Ubuntu + Windows, fmt, clippy, tests, deny, MSRV) | ✅ | Run [40373180391](https://github.com/Dnivaux/VMA/actions/runs/40373180391) vert sur les 2 OS (commit `46a4162`) |
| `core` : `FrameRate` rationnel, `Resolution`, `InputId` | ✅ | 7 tests unitaires (CI) |
| `clock` : horloge à cadence fixe, images manquées, stats lock-free, `tracing` | ✅ | 6 tests (CI) |
| `clock` : histogramme du retard au réveil (p50 / p99 / p99,9 / max), marge d'attente configurable — *amendement 4* | ✅ | 3 tests (CI [40378932947](https://github.com/Dnivaux/VMA/actions/runs/40378932947)) |
| `compositor` : mire WGSL, cibles de rendu, readback | ✅ | 4 tests golden / analytiques (CI : lavapipe, WARP) |
| `engine` : thread de rendu, triple buffer lock-free, contre-pression GPU | ✅ | 3 tests unitaires + `runs_one_second` (CI) |
| `engine` : invariant du triple buffer documenté + tests de stress CPU et GPU — *amendement 2* | ✅ | `producer_never_writes_the_held_slot`, `triple_buffer_gpu_stress` (CI [40378932947](https://github.com/Dnivaux/VMA/actions/runs/40378932947), lavapipe et WARP) |
| `engine` : perte du GPU (rendu arrêté, horloge maintenue, sortie en erreur), erreurs GPU comptées — *amendement 3* | ✅ | `device_lost_stops_rendering_not_the_clock` (CI [40378932947](https://github.com/Dnivaux/VMA/actions/runs/40378932947)) |
| Fenêtre eframe : Program + panneau Statistiques (dont retard au réveil, erreurs GPU) | ✅ | Lancement manuel : 50,00 fps, 0 manquée |
| Mode soak dans l'app (`--soak`, `--hostile`, rapport Markdown, code de sortie) | ✅ (manuel) | Soak hostile Linux de 10 min ci-dessous |
| **Acceptation : 10 min à 50 fps sans image manquée** | ✅ (manuel) | 600 s, 30 002 images, 0 manquée, 0 répétée — [`tests/soak/reports/2026-10-26-phase0-render-loop.md`](tests/soak/reports/2026-10-26-phase0-render-loop.md) |
| **Acceptation : soak hostile 10 min, 0 image manquée — Linux** (amendement 3) | ✅ (manuel) | 600 s, 30 001 images, 0 manquée, 0 répétée, réveil p99 3 µs / max 1,4 ms, UI gelée jusqu'à 26,9 s sans effet sur le Program — [`tests/soak/reports/2026-10-26-phase0-hostile-linux.md`](tests/soak/reports/2026-10-26-phase0-hostile-linux.md) |
| **Acceptation : soak hostile 10 min, 0 image manquée — Windows** | ⏸ | **Reporté** (2026-10-26, décision du mainteneur) : exige de redémarrer la machine de dev sous Windows. À passer au prochain démarrage sous Windows, **au plus tard avec le soak d'acceptation de la Phase 1 sous Windows**. Couvert entre-temps : build, clippy et 32 tests sur Windows en CI (WARP). Checklist : [`docs/manual-tests/phase0-soak-hostile.md`](docs/manual-tests/phase0-soak-hostile.md) |
| **Acceptation : CI verte sur les 2 OS** | ✅ | Runs 40373180391 et [40378932947](https://github.com/Dnivaux/VMA/actions/runs/40378932947) (32 tests, Ubuntu + Windows) |
| Doc : `CLAUDE.md`, ADR 0001 et 0002, specs `clock` / `compositor` / `engine`, `docs/dev-setup.md`, checklist soak hostile | ✅ | — |

« Local » = Arch Linux, RTX 5090, Rust 1.99.1.

### Tests ignorés / manuels
| Test | Raison |
|---|---|
| `vma-engine::render_loop::soak_render_loop` | 10 min, lancé manuellement sur la machine de dev ; rapport dans `tests/soak/reports/` |
| Soak hostile (`vma --soak 600 --hostile`) | Fenêtre réelle et actions à la souris : impossible sur les runners CI ; checklist `docs/manual-tests/phase0-soak-hostile.md`. Linux : passé. **Windows : reporté** (voir ci-dessus) |

### Dette connue (voir ADR 0001)
- Allocations et verrous **internes à wgpu** dans la boucle de rendu (pas d'allocation applicative).
- Thread de rendu en priorité OS normale.
- Temps GPU et utilisation GPU non mesurés (seul le temps CPU de rendu l'est).
- Couleur : R'G'B' encodé en `Rgba8Unorm`, pas encore de cibles `Rgba16Float`.
- Perte du GPU : pas de recréation à chaud (Phase 2).
- Soak hostile sous Wayland : déplacement et dé-minimisation automatiques sans effet (faits à la main).
- **Soak hostile Windows non passé** : le risque principal non vérifié est le gel du thread UI
  pendant la boucle modale de déplacement / redimensionnement de Windows, combiné à la `Queue`
  partagée (contention possible sur les verrous internes de wgpu au moment du `present`).

## Phase 1 — Mélange de base · ⬜
Inputs GStreamer (webcam / capture, fichier en boucle, image, couleur), Preview + Program,
Cut / Fade (mixage gamma par défaut, linéaire en option), couche `Command`, cibles internes
`Rgba16Float` + YUV → RGB, **cadence maître réglable (25 / 29,97 / 30 / 50 / 59,94 / 60p) avec
rééchantillonnage des inputs** (ADR 0002).

**Spec et plan validés le 2026-10-26** : [`docs/specs/phase-1.md`](docs/specs/phase-1.md)
(critères A1–A20, plan en 10 étapes), specs `commands`, `io-gst`, `compositor`, `ui`, ADR 0003
(état / commandes / threads), 0004 (GStreamer), 0005 (chaîne couleur) acceptés.
Ajouts « alignement sur la référence » (§7 : vignettes live, Quick Play, 4 boutons de
transition, T-Bar, contrôles vidéo) **validés** le même jour. Observation de la référence faite
le 2026-10-26 ([`docs/manual-tests/observation-reference-phase1.md`](docs/manual-tests/observation-reference-phase1.md)) :
tout confirmé sauf « Cut pendant un Fade » (ignoré, corrigé) ; reste à relever les durées par
défaut des 4 boutons et le sens de « forcer le fade à cut » (questions de suivi).

| Étape (plan §5) | État | Preuve |
|---|---|---|
| 1. `core` : `PixelFormat`, `ColorInfo`, `Rgb8`, `InputKind`, 29,97p, `serde` | ✅ | 21 tests `vma-core` ; CI verte Ubuntu + Windows ([run 40394934963](https://github.com/Dnivaux/VMA/actions/runs/40394934963)) |
| 2. `engine` : `Command`, `State`, `vma-control`, journal, `RenderPlan` | ✅ | 40 nouveaux tests unitaires `vma-engine` (temps simulé) : A3 (Cut au tick suivant), A4 côté moteur (6 cadences × 1 / 40 / 500 / 1000 ms), A15–A18, refus sans effet, rejeu du journal ; test de bout en bout avec les vrais threads (10/10 en local) ; CI verte Ubuntu + Windows ([run 40471756097](https://github.com/Dnivaux/VMA/actions/runs/40471756097)). A11 partiel : journal fait, l'UI n'envoie pas encore de commandes (étape 4) |
| 3. `compositor` : `Rgba16Float`, sortie `Rgba8Unorm`, couleur unie, Cut / Fade | ✅ | 8 golden `compositor::golden_mix` (A2, A4, A18 côté compositeur ; gamma et linéaire ; chaîne sans perte) et `render_loop::program_pixels_follow_cut_and_fade`, verts sur RTX 5090 et Intel ARL ; soak 60 s 1080p50 : 0 manquée ; CI verte (lavapipe / WARP, [run 40471756097](https://github.com/Dnivaux/VMA/actions/runs/40471756097)) |
| 4. `engine` + `ui` : Preview + Program, tally, barre d'inputs, boutons, T-Bar, raccourcis | ✅ | **Premier jalon visible.** 10 tests d'UI sans fenêtre (A11, A17 côté UI : chaque bouton, menu, double-clic, T-Bar, raccourci émet la bonne `Command`), tally, vignettes et Preview vérifiées sur GPU ; soak de l'appli 10 s : 0 manquée. Checklist manuelle [`docs/manual-tests/phase1-inputs.md`](docs/manual-tests/phase1-inputs.md) à passer. Contrôles vidéo reportés à l'étape 8. CI verte ([run 40471756097](https://github.com/Dnivaux/VMA/actions/runs/40471756097)) après correction des vignettes de couleur unie sous lavapipe (`5f927d4` : la *blend constant* y était fausse) |
| 5. `compositor` : upload CPU (NV12, I420, YUY2, UYVY, RGBA, BGRA), YUV → RGB, échelle, image fixe | ✅ | `InputFrame` + `Source::Frame` (Program, Preview, vignettes). 7 golden `compositor::golden_frame` : **A7** (24 combinaisons format × matrice × plage, ±1, écart max aux barres idéales : 1 code), RGBA / BGRA bit-exact, pillarbox 4:3 ±1 px, damier réduit = 191 (échelle en linéaire), tailles impaires, uploads invalides refusés ; verts sur RTX 5090 et Intel ARL. Image fixe : décodage PNG / JPEG (`io-gst::still`, orientation EXIF, limite 8192 px, 4 tests) ; **affichage d'un input image avec `vma-io` (étape 7)** : les feeds existent depuis l'étape 6, il manque le thread qui décode et pousse l'image. CI verte Ubuntu + Windows ([run 40472932225](https://github.com/Dnivaux/VMA/actions/runs/40472932225)) |
| 6. `engine` : feeds d'images par input (triple buffer de textures), sélection par tick, stats par input, ajout / retrait à chaud | ✅ | Module `feed` + `FeedRegistry` / `FrameSink`, attache par `vma-control` (`Event::InputLive`), retour des feeds retirés vers `vma-control`. **A5** côté moteur : 36 combinaisons source × maître en temps simulé (image exacte à chaque tick, comptes exacts) + 3 tests du feed ; **A12** : 20 ajouts / retraits pendant le rendu, 0 image manquée (`render_loop::hot_add_and_remove_without_missed_frames`, 10/10 en local) ; feed visible au Program, en Preview et en vignette ; verts sur RTX 5090 et Intel ARL. Section « Inputs » du panneau Statistiques. Soak de l'appli 10 s : 0 manquée. Image fixe à l'écran : avec `vma-io` (étape 7). CI verte Ubuntu + Windows, A12 compris sur lavapipe et WARP ([run 40475582428](https://github.com/Dnivaux/VMA/actions/runs/40475582428)) |

## Phase 2 — Sorties · ⬜
## Phase 3 — Réseau et audio · ⬜
## Phase 4 — Habillage · ⬜
## Phase 5 — Contrôle et monitoring · ⬜
## Phase 6 — Sport · ⬜
