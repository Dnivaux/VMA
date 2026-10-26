# CLAUDE.md — conventions du projet Aiguillage

Résumé opérationnel de `BRIEF.md` (qui reste la référence). À lire au début de chaque session.

## Le projet en une phrase
Mélangeur vidéo live open source en Rust (switcher, streaming, enregistrement), multiplateforme
(Linux + Windows, macOS plus tard). Nom de code provisoire : **Aiguillage**.

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
cargo test --release -p aiguillage-engine --test render_loop -- --ignored --nocapture
AIGUILLAGE_SOAK_SECS=60 cargo test --release -p aiguillage-engine --test render_loop -- --ignored --nocapture

# Regénérer les golden frames après un changement visuel VOULU (puis relire le PNG) :
AIGUILLAGE_UPDATE_GOLDEN=1 cargo test -p aiguillage-compositor --test golden_test_pattern
```
Variables wgpu utiles : `WGPU_BACKEND=vulkan|dx12` ; `WGPU_ADAPTER_NAME=Intel` (tests headless) ; `WGPU_POWER_PREF=low` (app).

## Structure
| Chemin | Rôle |
|---|---|
| `crates/core` | Types communs (`FrameRate` rationnel, `Resolution`, `InputId`). **Ne pas modifier sans le signaler** : tout en dépend. |
| `crates/clock` | Horloge maître à cadence fixe, compteur d'images manquées. |
| `crates/compositor` | wgpu/WGSL : mire, cibles de rendu, readback (tests). |
| `crates/engine` | Thread de rendu cadencé par l'horloge, triple buffer vers l'UI, stats. (State store + `Command` en Phase 1.) |
| `crates/ui` | egui : vue Program, panneau Statistiques. |
| `app/` | Binaire : assemble fenêtre eframe, moteur, UI. |
| `crates/{io-gst,io-ndi,io-html,audio,replay,api}` | Squelettes vides pour les phases suivantes. |
| `docs/adr/` | Décisions d'architecture (une par fichier, à faire valider). |
| `docs/specs/` | Une spec par module, avec critères d'acceptation. |
| `docs/manual-tests/` | Checklists de tests sur matériel réel. |
| `tests/golden/` | Images de référence. `tests/soak/reports/` : rapports de soak. |

## Conventions
- Identifiants de code en anglais ; docs et textes d'UI en français.
- Commits : anglais, Conventional Commits (`feat(clock): …`), un commit = une fonctionnalité testée.
- `unsafe` interdit par défaut (`unsafe_code = "deny"`) ; s'il devient nécessaire (NDI, interop),
  l'autoriser localement avec un commentaire `// SAFETY:` justifié.
- Les tests d'intégration autorisent explicitement `unwrap`/`panic` en tête de fichier.
- Couleur Phase 0 : textures Program `Rgba8Unorm`, valeurs R'G'B' Rec.709 encodées, pleine plage.

## Méthode (BRIEF §10)
- Avant une phase : spec dans `docs/specs/` + plan, **attendre la validation**.
- Toute décision structurante (dépendance lourde, archi, format de fichier) → ADR à valider.
- Une ambiguïté de comportement → poser la question au mainteneur plutôt que deviner.
- Pas de fonctionnalité hors de la phase courante, pas de refactor massif non demandé.
- Parallélisation : une fois les types de `core` figés, un agent par crate dans des git worktrees.
