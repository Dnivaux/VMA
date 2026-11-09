# ADR 0010 — Alpha, overlays et stinger

- **Statut** : **proposé** (à valider avec la spec de la Phase 4)
- **Date** : 2026-11-09
- **Contexte** : BRIEF §4 (Phase 4 : 4 overlay channels, stinger, golden-frame sur l'alpha),
  §5 (rendu cadencé par le maître, dernière image de chaque input) ; ADR 0001 (calques
  `Rgba16Float`), ADR 0004 (entrées), ADR 0005 (couleur : valeurs encodées, mélange en gamma
  par défaut) ; spec [`docs/specs/phase-4.md`](../specs/phase-4.md)

## Constat
Le shader de conversion des images rend α = 1 : l'alpha de toute source est perdu. Le Program
est composé de deux calques mélangés (Cut, Fade, T-Bar), puis l'étage de sortie (FTB, NV12).

## Décision
1. **Alpha prémultiplié partout dans le compositeur.** Chaque image porte un `AlphaMode`
   (`core`) : `Opaque` (toutes les sources actuelles : α = 1), `Straight` (PNG, ProRes 4444,
   WebM alpha : multiplié par le shader de conversion), `Premultiplied` (page HTML, si l'essai
   de l'ADR 0009 le confirme). Les calques `Rgba16Float` gardent l'alpha.
2. **Composition sur les valeurs encodées** (domaine gamma) : `out = src + (1 − α) · dst`,
   prémultiplié. C'est le comportement d'un keyer de régie et celui des pages web ; pas
   d'option linéaire en Phase 4. Le scaling d'une source avec alpha se fait sur les valeurs
   prémultipliées (pas de franges).
3. **Ordre des couches**, à chaque tick : Program (mélange A / B) ▸ stinger ▸ overlays 1 à 4 ▸
   étage de sortie (FTB, puis NV12 ou `Rgba8Unorm`). Les sorties restent **opaques** : l'alpha
   du Program n'est pas transmis.
4. **Overlay channels** : 4 calques supplémentaires de la taille du Program, remplis comme
   les autres (source → calque), puis une passe « over » par channel actif, avec l'opacité de
   sa transition (Cut : 0 ou 1 au tick ; Fade : linéaire sur sa durée, au tick près comme le
   Fade du Program). Un channel inactif ne coûte rien (ni conversion ni passe). Tous les objets
   GPU sont créés au démarrage (règle du thread de rendu).
5. **Stinger** : une source **séquentielle**, pas « dernière image » : un pipeline préchargé
   par emplacement (2), qui décode en avance dans un anneau d'images (16) ; au déclenchement au
   tick *t₀*, le rendu prend l'image *k* au tick *t₀ + k*. Au tick de l'image de bascule, le
   mélange du Program passe de A à B (Cut sous le stinger) ; à la fin du fichier, le stinger
   disparaît et le pipeline revient au début (préchargé pour la suivante). Image *k* pas encore
   décodée : la précédente est répétée et comptée ; la bascule a lieu à son tick quoi qu'il
   arrive. Le son du fichier passe par un anneau audio comme un input (ADR 0007), au Master.
6. **Golden frames** : sources RGBA synthétiques (α = 0, ¼, ½, 1 ; droit et prémultiplié) sur
   fond connu, comparées **au code près** (pas seulement au PSNR) ; les golden des Phases 1 à 3
   ne changent pas.

## Options écartées
- **Alpha droit dans les calques** : chaque passe redivise par α, franges au scaling.
- **Composition en lumière linéaire** : différente de ce que montrent les pages web et les
  keyers habituels ; ajoutable plus tard comme le Fade linéaire.
- **Stinger comme input « dernière image »** : il sauterait ou répéterait des images selon la
  phase des horloges, et le point de bascule ne tomberait pas à l'image près.
- **Stinger entièrement décodé en mémoire au chargement** : 1080p RGBA ≈ 8,3 Mo par image,
  soit ≈ 830 Mo pour 2 s à 50p par emplacement.

## Conséquences
- `core` : `AlphaMode`, `OverlayChannel`, `InputKind::Html` (ajouts).
- Coût de rendu : jusqu'à 4 conversions et 4 passes de plus par tick (+ 1 pour le stinger),
  mesuré dans le rapport de soak (D10, D11).
- Les sources avec alpha droit converties en RGBA par GStreamer (CPU) en Phase 4 ; un upload
  YUVA direct (A420) viendra si la mesure le demande.
