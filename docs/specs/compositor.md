# Spec — `compositor` (Phase 0)

## Comportement
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

## Couleur
R'G'B' Rec.709 encodé, pleine plage, dans `Rgba8Unorm`. Linéaire + YUV en Phase 1.

## Critères d'acceptation
| Critère | Test | Statut |
|---|---|---|
| Barres à 191 exactement, alpha 255 | `bars_have_exact_75_percent_values` | CI |
| Rampe monotone noir → blanc | `ramp_is_monotonic_from_black_to_white` | CI |
| Marqueur position(f0)=0, position(f50)=150 px, périodique | `marker_moves_with_frame_index` | CI |
| Golden frame 320×180 f0, PSNR ≥ 45 dB | `golden_frame_0` | CI |

Mesuré en local : bit-exact sur RTX 5090 (Vulkan), 73 dB sur Intel ARL (Vulkan, Mesa).
