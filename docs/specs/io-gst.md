# Spec — `io-gst` : inputs (Phase 1)

- **Statut** : proposé, à valider avec [`phase-1.md`](phase-1.md) et l'ADR 0004

## Types d'input
| Input | Pipeline / source | Notes |
|---|---|---|
| Capture (webcam, carte) | élément créé par `gst::DeviceMonitor` (classe `Video/Source`) → `decodebin` si compressé (MJPEG) → caps → appsink | Linux : V4L2 (les doublons PipeWire sont masqués) ; Windows : Media Foundation (`mfvideosrc`) |
| Fichier vidéo | `uridecodebin` → vidéo → caps → appsink ; audio vers `fakesink` (Phase 3) | **boucle sans trou** : seek de segment au démarrage, puis seek non-flush à 0 sur `SEGMENT_DONE` |
| Image fixe | décodée une fois par `image` (PNG, JPEG) sur `vma-io`, uploadée une fois | pas de pipeline |
| Couleur unie | aucune ressource : couleur passée au shader | — |
| Source de test | `videotestsrc is-live=true pattern=…` à la cadence / résolution demandées | CI, soaks, tests de cadence |

**Formats acceptés à l'appsink** (pas de conversion CPU) : `NV12`, `I420`, `YUY2`, `UYVY`,
`RGBA`, `BGRA`. Tout autre format passe par `videoconvert` vers `NV12` (compté dans les stats).
**Colorimétrie** lue dans les caps (matrice 601 / 709, plage limitée / pleine) ; si absente :
709 pour une hauteur ≥ 720, sinon 601 ; plage limitée pour du YUV, pleine pour du RGB.

**Mode de capture** : choisi automatiquement parmi ceux annoncés par le périphérique :
résolution du Program si possible (sinon la plus grande inférieure), cadence la plus proche
au-dessus de la cadence maître, format brut préféré au MJPEG si la cadence le permet. Le mode
choisi est affiché dans l'UI.

## Transport des images
- Par input, 3 jeux de textures (un par plan : NV12 = Y `R8` + UV `Rg8` ; I420 = 3 × `R8` ;
  YUY2 / UYVY = `Rgba8` de largeur w/2 ; RGBA / BGRA = `Rgba8` / `Bgra8`), créés par
  `vma-control` quand les caps sont connues.
- Callback `new-sample` (thread de streaming) : `write_texture` de chaque plan dans le slot
  « back » (pas de copie intermédiaire côté application), métadonnées en atomiques (PTS source,
  heure d'arrivée monotone, n° d'image source), puis publication (triple buffer d'indices).
- `appsink` : `max-buffers=1`, `drop=true` ; `sync=true` pour un fichier (lecture à vitesse
  réelle), `sync=false` pour une capture ou une source live (latence minimale).
- **Changement de caps** en cours de route (résolution, format) : nouvelles textures demandées
  à `vma-control` ; en attendant, l'input garde sa dernière image.

## Rééchantillonnage sur l'horloge maître
À chaque tick, le rendu prend **la dernière image publiée** de chaque input (BRIEF §5) : une
source plus lente est répétée, une plus rapide voit des images sautées. Stats par input :
images reçues / s, répétées, sautées (écrasées avant d'être vues), latence arrivée → tick
(histogramme, p50 / p99 / max). Le rééchantillonnage par PTS avec une image de latence (moins
de saccades sur une source irrégulière) est une évolution possible, hors Phase 1.

## Signal perdu et reconnexion
- `SignalLost` si : erreur du pipeline, EOS d'une capture, ou aucune image depuis
  `max(500 ms, 10 périodes de la source)`.
- Le rendu affiche du **noir** à la place de l'input (*question 3* de `phase-1.md`) ; l'UI montre
  l'état et le nombre de tentatives.
- Reconnexion automatique, sans fin : pipeline détruit puis recréé après 1 s, 2 s, 5 s, puis
  toutes les 10 s. Un périphérique de capture qui réapparaît est reconnu par son identifiant
  stable (chemin du périphérique).
- Aucune erreur GStreamer ne remonte en panique : tout passe en `IoEvent` typés (`thiserror`).

## Interface (esquisse)
```rust
pub struct IoRuntime;            // possède le thread vma-io
impl IoRuntime {
    pub fn start(events: Sender<IoEvent>) -> Result<Self, IoError>;  // échec => GStreamer indisponible
    pub fn devices(&self) -> Vec<CaptureDevice>;                      // nom affiché + DeviceId stable
    pub fn open(&self, input: InputId, spec: &InputSpec, sink: FrameSink) -> Result<(), IoError>;
    pub fn close(&self, input: InputId);
}
pub enum IoEvent { Caps { input, caps }, Live { input }, SignalLost { input, reason }, … }
```
`FrameSink` est fourni par `engine` (textures + indices + métadonnées) ; `io-gst` ne connaît pas
le compositeur.

## Critères d'acceptation
| Critère | Test | Où |
|---|---|---|
| `videotestsrc` 1080p50 → images publiées dans l'ordre, au bon rythme (± 2 %) | intégration | CI |
| Chaque format accepté arrive sans `videoconvert` ; un format non listé passe par lui | intégration (caps forcées) | CI |
| Fichier en boucle : ≥ 3 boucles, n° d'image continu modulo la durée, 0 image noire (A9) | fichier MKV généré par le test | CI |
| Erreur forcée du pipeline → `SignalLost`, puis reconnexion → `Live` (A8) | élément factice qui échoue | CI |
| Débrancher / rebrancher la webcam pendant le Program | checklist `docs/manual-tests/phase1-inputs.md` | manuel |
| GStreamer absent → l'application démarre, inputs capture / fichier désactivés avec message | test avec initialisation simulée en échec | CI |
| Mode de capture choisi selon les règles ci-dessus | test unitaire sur des listes de caps | CI |
