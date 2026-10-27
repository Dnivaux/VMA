# Spec — `compositor`

## Phase 0 — Comportement
- `RenderTarget` : texture hors écran `Rgba8Unorm` (`RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC`).
- `TestPattern` : mire générée par shader WGSL (triangle plein écran, aucun buffer de sommets).
  - 2/3 haut : 8 barres 75 % (100/0/75/0) : blanc, jaune, cyan, vert, magenta, rouge, bleu, noir
    → valeur 191 sur 8 bits.
  - 1/6 : rampe de gris 0 → 255.
  - 1/6 bas : marqueur blanc (1/16 de largeur) qui traverse l'écran en 100 images (2 s à 50p) :
    une image figée ou sautée se voit à l'œil.
  - Déterministe pour (index d'image, résolution).
- `encode()` : enregistre la mire dans un `CommandEncoder` ; ne bloque pas, pas d'allocation applicative.
- `headless::request_gpu()` : device sans fenêtre (respecte `WGPU_BACKEND`, `WGPU_ADAPTER_NAME`).
- `readback::read_rgba8()` : copie GPU → CPU **bloquante**, tests uniquement.

## Phase 0 — Couleur
R'G'B' Rec.709 encodé, pleine plage, dans `Rgba8Unorm`. Linéaire + YUV en Phase 1.

## Phase 0 — Critères d'acceptation
| Critère | Test | Statut |
|---|---|---|
| Barres à 191 exactement, alpha 255 | `bars_have_exact_75_percent_values` | CI |
| Rampe monotone noir → blanc | `ramp_is_monotonic_from_black_to_white` | CI |
| Marqueur position(f0)=0, position(f50)=150 px, périodique | `marker_moves_with_frame_index` | CI |
| Golden frame 320×180 f0, PSNR ≥ 45 dB | `golden_frame_0` | CI |

Mesuré en local : bit-exact sur RTX 5090 (Vulkan), 73 dB sur Intel ARL (Vulkan, Mesa).

---

## Phase 1 — Preview, Program, transitions, YUV (validé le 2026-10-26)
Voir [`phase-1.md`](phase-1.md) et l'ADR 0005 (chaîne couleur).

### Chaîne de rendu par tick
Les inputs **à l'antenne ou en Preview** (au plus 2 en Phase 1, transition comprise) sont
traités à chaque tick à pleine résolution. Les **vignettes** de la barre d'inputs sont rendues à
part (voir plus bas).

1. **Passe d'entrée** (par input visible) : échantillonne les plans (NV12, I420, YUY2, UYVY,
   RGBA, BGRA), applique plage et matrice (601 / 709) → R'G'B'. Si la taille diffère de celle du
   Program : linéarise (EOTF BT.1886), écrit dans une texture `Rgba16Float` à la taille native.
2. **Passe d'échelle** (si nécessaire) : échantillonnage bilinéaire de la texture linéaire
   (= filtrage en lumière linéaire), **fit** avec bandes noires, ré-encodage en R'G'B' →
   « calque » `Rgba16Float` à la taille du Program. Sans changement de taille, la passe 1 écrit
   directement le calque, sans linéarisation (aller-retour inutile).
   - Couleur unie : calque rempli directement. Input absent ou en signal perdu : noir.
3. **Passe de mixage** → cibles `Rgba16Float` Program et Preview :
   - hors transition : copie du calque ;
   - Fade, domaine **gamma** (défaut) : `mix(from, to, p)` sur les valeurs encodées ;
   - Fade, domaine **linéaire** (option) : décodage, `mix`, ré-encodage.
4. **Passe de sortie** : `Rgba16Float` → `Rgba8Unorm` (arrondi au plus proche) dans les triple
   buffers Program et Preview lus par l'UI (invariant ADR 0001), et plus tard vers les sorties.

5. **Vignettes** : pour chaque input, une vignette `Rgba8Unorm` 320 × 180 (passes 1 et 2 à
   petite taille, fit) rendue **un tick sur deux**, inputs répartis sur les ticks pour lisser
   la charge ; publiées vers l'UI par un triple buffer de l'ensemble des vignettes (même
   invariant). Un input sans image ou en signal perdu donne une vignette noire.

Toutes les ressources (textures intermédiaires, pipelines, bind groups) sont créées hors du
thread de rendu (`vma-control`) ; le rendu ne fait qu'encoder. Downscale fort (4K → 1080p) en
simple bilinéaire en Phase 1 (crénelage possible) ; mipmaps ou filtre plus large plus tard.

### Mise en œuvre (étape 3 du plan, 2026-10-26)
- `Mixer` (un par sortie : Program maintenant, Preview à l'étape 4) : deux calques
  `Rgba16Float`, la cible `program16` (`Rgba16Float`, que liront les sorties de la Phase 2), et
  les pipelines ; tout est créé dans `Mixer::new`, `encode` ne fait qu'enregistrer 4 passes
  et écrire un uniform de 16 octets.
- Sources de la Phase 1 : `Black`, `Color(Rgb8)` (calque rempli par l'opération de *clear*),
  `TestPattern { frame }` (la mire, rendue directement dans le calque). Le second calque n'est
  rempli que si son poids est non nul.
- Passe de sortie : arrondi **explicite** dans le shader (`round(v × 255) / 255`), pour ne pas
  dépendre de la conversion flottant → unorm propre à chaque GPU.
- Mesures : chaîne flottante sans perte pour les 256 codes ; mire via le `Mixer` à 74 dB du
  golden Phase 0 (écart d'un code sur quelques points de la rampe) ; Fade 50 % mire ↔ couleur
  à 62 dB ; RTX 5090 et Intel ARL (Vulkan). Soak de 60 s en 1080p50 : 0 image manquée, CPU de
  rendu 86 µs en moyenne.
- **Vignettes (étape 4)** : `thumbnails::ThumbnailPainter` dessine chaque input dans sa
  cellule d'un atlas `Rgba8Unorm` de 8 × 8 cellules de 320 × 180 (2560 × 1440), en une passe :
  viewport par cellule, couleur unie lue dans un uniform unique de 64 couleurs (une par
  cellule, écrit une fois par atlas, indexé par l'instance du draw ; aucun buffer par input),
  mire par son pipeline. *(Première version par la blend constant abandonnée le 2026-10-27 :
  lavapipe / Mesa 25.2 la restitue fausse, rouge lu `[255, 255, 0, 0]`.)* Pas de *clear* de l'atlas (coûteux en rendu logiciel) : le lecteur ne regarde
  que les cellules annoncées. **Tous** les inputs sont redessinés un tick sur deux (plutôt que
  répartis sur les ticks : un atlas publié doit être complet ; à revoir avec les inputs vidéo
  si la charge l'exige). Test : `golden_mix::thumbnail_atlas_cells` (mire en vignette = golden).
- **Frames CPU (étape 5, 2026-10-27)** : `InputFrame` (module `frame`) = une texture par plan
  (NV12 = `R8` + `Rg8`, I420 = 3 × `R8`, YUY2 / UYVY = `Rgba8` de demi-largeur, RGBA / BGRA =
  `Rgba8` / `Bgra8`) + une texture `Rgba16Float` à la taille native (lumière d'affichage) ;
  `write` (plans avec *stride*, validés avant tout upload : erreur typée `FrameError`) et
  `write_packed`. Pipelines et layouts partagés dans `FramePipelines` (créé une fois, cloné à
  coût nul). `Source::Frame(&InputFrame)` dans le `Mixer` et les vignettes.
  - Même taille que la cible : passe directe planes → R'G'B' dans le calque.
  - Sinon : planes → lumière d'affichage (constante de pipeline `LINEARIZE`), puis
    échantillonnage bilinéaire dans le rectangle `fit_rect` (viewport, pixels entiers, arrondi
    au plus proche, pixels carrés), ré-encodage ; bandes noires par le *clear* du calque (ou
    le fond noir de la cellule de vignette).
  - Chroma 4:2:0 / 4:2:2 : échantillon co-situé (au plus proche), pas d'interpolation — à
    revoir si les contours colorés se voient sur de la vraie vidéo.
  - RGB : colorimétrie ignorée (toujours pleine plage). Valeurs hors 0–1 après matrice :
    écrêtées.
  - Pas de déduplication : une frame visible dans le Program, la Preview et les vignettes est
    convertie pour chacune (coût GPU faible à 4 inputs ; à revoir si besoin).
- Le golden « Fade 50 % mire ↔ couleur » est **analytique** (moyenne du golden de la mire et de
  la couleur), plutôt qu'un nouveau PNG `fade50.png`.

### Critères d'acceptation (Phase 1)
| Critère | Test | Où |
|---|---|---|
| **Fade 50 % entre deux couleurs** = moyenne des valeurs encodées, ±1 (A2) | `golden_mix::fade_50_percent_gamma_is_the_mean_of_the_codes` (±0,5) | CI |
| Fade 50 % en domaine linéaire = `encode((decode(a)+decode(b))/2)`, ±1 | `golden_mix::fade_50_percent_linear_mixes_display_light` | CI |
| Fade 0 % / 100 % = source / destination exactes (A4) | `golden_mix::fade_at_0_and_100_percent_is_exact` | CI |
| T-Bar à 128 = mix à 128/255 (A18) | `golden_mix::t_bar_at_128_mixes_128_of_255` | CI |
| Chaîne `Rgba16Float` → `Rgba8Unorm` sans perte pour les 256 codes | `golden_mix::every_code_value_survives_the_float_chain` | CI |
| Mire via le `Mixer` = golden Phase 0, PSNR ≥ 45 dB | `golden_mix::test_pattern_through_the_float_chain_matches_the_phase_0_golden` | CI |
| Vignette d'une mire = mire réduite (PSNR ≥ 35 dB contre une réduction de référence) ; vignette noire en signal perdu | golden | CI |
| Fade 50 % mire ↔ couleur : PSNR ≥ 45 dB contre l'image de référence | `golden_mix::fade_50_percent_pattern_to_color` (référence analytique) | CI |
| YUV → RGB : barres 75 % synthétiques en NV12, I420, YUY2, UYVY, 601 / 709, limitée / pleine → R'G'B' attendu ±1 (A7) | `golden_frame::ycbcr_bars_decode_for_every_format_matrix_and_range` (24 combinaisons, RGBA / BGRA compris ; ±1 contre la référence CPU **et** contre les barres idéales 191 / 0) | CI |
| Sans changement de taille, passe d'entrée sans perte : RGBA → RGBA bit-exact | `golden_frame::rgb_at_program_size_is_bit_exact` (RGBA et BGRA) | CI |
| Fit : source 4:3 dans un Program 16:9 → bandes noires de la bonne largeur, ±1 px | `golden_frame::four_by_three_is_pillarboxed`, `frame_thumbnail_is_fitted_in_its_cell` | CI |
| Mise à l'échelle linéaire : damier noir / blanc 1 px réduit de moitié → gris 191 ±1 (`255 × 0,5^(1/2,4)` : moitié de la luminance), et non 128 | `golden_frame::halved_checkerboard_is_191_not_128` | CI |
| Tailles impaires : dernière colonne et dernière ligne présentes (chroma arrondie au supérieur) | `golden_frame::odd_sized_frames_are_complete` | CI |
| Upload invalide (nombre de plans, *stride*, taille) refusé sans erreur GPU ni panique | `golden_frame::bad_uploads_are_refused` | CI |
