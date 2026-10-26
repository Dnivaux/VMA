# ROADMAP — état réel

Règles (BRIEF §2.5) : pas de pourcentage. Une fonctionnalité est **faite** uniquement si ses
tests d'acceptation passent en CI. Les tests désactivés ou manuels sont listés explicitement.

Légende : ✅ fait (tests verts) · 🟡 implémenté, validation incomplète · ⬜ pas commencé

---

## Phase 0 — Fondations · *en revue*

| Élément | État | Preuve |
|---|---|---|
| Workspace Cargo (12 crates, édition 2024, MSRV 1.95) | ✅ | `cargo +1.95 check --workspace --all-targets` local |
| Lints : clippy `-D warnings`, `unwrap`/`expect`/`panic` interdits hors tests | ✅ | `cargo clippy --workspace --all-targets -- -D warnings` local |
| Format (`rustfmt`) | ✅ | `cargo fmt --check` local |
| Politique de licences (`cargo deny`) | ✅ | `cargo deny check` local : ok (3 advisories « unmaintained » et les polices d'egui en exception justifiée dans `deny.toml`) |
| CI GitHub Actions (Ubuntu + Windows, fmt, clippy, tests, deny, MSRV) | 🟡 | Workflow écrit, **jamais exécuté** : le dépôt n'a pas encore de remote GitHub |
| `core` : `FrameRate` rationnel, `Resolution`, `InputId` | ✅ | 7 tests unitaires (local) |
| `clock` : horloge à cadence fixe, images manquées, stats lock-free, `tracing` | ✅ | 6 tests (local) |
| `compositor` : mire WGSL, cibles de rendu, readback | ✅ | 4 tests golden / analytiques (local, RTX 5090 et Intel ARL) |
| `engine` : thread de rendu, triple buffer lock-free, contre-pression GPU | ✅ | 3 tests unitaires + `runs_one_second` (local) |
| Fenêtre eframe : Program + panneau Statistiques | ✅ | Lancement manuel : 50,00 fps, 0 manquée |
| **Acceptation : 10 min à 50 fps sans image manquée** | ✅ (manuel) | 600 s, 30 002 images, 0 manquée, 0 répétée — [`tests/soak/reports/2026-10-26-phase0-render-loop.md`](tests/soak/reports/2026-10-26-phase0-render-loop.md) |
| **Acceptation : CI verte sur les 2 OS** | ⬜ | En attente du dépôt GitHub |
| Doc : `CLAUDE.md`, ADR 0001, specs `clock` / `compositor` / `engine`, `docs/dev-setup.md` | ✅ | — |

« Local » = Arch Linux, RTX 5090, Rust 1.99.1. Tant que la CI n'a pas tourné, les ✅ ci-dessus
ne sont pas encore « faits » au sens strict du BRIEF.

### Tests ignorés / manuels
| Test | Raison |
|---|---|
| `vma-engine::render_loop::soak_render_loop` | 10 min, lancé manuellement sur la machine de dev ; rapport dans `tests/soak/reports/` |

### Dette connue (voir ADR 0001)
- Allocations et verrous **internes à wgpu** dans la boucle de rendu (pas d'allocation applicative).
- Thread de rendu en priorité OS normale.
- Temps GPU et utilisation GPU non mesurés (seul le temps CPU de rendu l'est).
- Couleur : R'G'B' encodé en `Rgba8Unorm`, pas encore de compositing linéaire.

## Phase 1 — Mélange de base · ⬜
Inputs GStreamer (webcam / capture, fichier en boucle, image, couleur), Preview + Program,
Cut / Fade, couche `Command`, compositing linéaire + YUV → RGB.
Décisions à prendre avant : cadences du MVP (50p seul ou multi-cadences), UI (BRIEF §12).

## Phase 2 — Sorties · ⬜
## Phase 3 — Réseau et audio · ⬜
## Phase 4 — Habillage · ⬜
## Phase 5 — Contrôle et monitoring · ⬜
## Phase 6 — Sport · ⬜
