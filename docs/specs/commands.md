# Spec — couche de commandes et état (`engine`, Phase 1)

- **Statut** : validé (2026-10-26), complété par l'alignement sur la référence
  ([`phase-1.md`](phase-1.md) §7, validé le 2026-10-26)

## Interface publique (esquisse)
```rust
pub enum Command {
    AddInput { spec: InputSpec, name: Option<String> },
    RemoveInput { input: InputId },
    SetPreview { input: InputId },
    PreviewNext, PreviewPrevious,
    /// Preview ↔ Program, sans transition.
    Cut,
    /// Envoie directement un input au Program sans changer la Preview.
    CutDirect { input: InputId },
    /// Transition du bouton n (1–4) : Preview → Program avec l'effet et la durée du bouton.
    Transition { button: u8 },
    /// Transition Quick Play (Fade 500 ms par défaut). Avec `input` : cet input part au
    /// Program directement (sans passer par la Preview).
    QuickPlay { input: Option<InputId> },
    /// T-Bar : 0 = Program seul, 255 = transition terminée (effet du bouton 1).
    SetFader { value: u8 },
    SetTransition { button: u8, effect: Effect, duration_ms: u32 },   // Effect: Fade | Cut
    SetQuickPlay { effect: Effect, duration_ms: u32 },
    SetMixDomain { domain: MixDomain },      // Gamma (défaut) | Linear
    // Réglages d'un input
    RenameInput { input: InputId, name: String },
    SetColor { input: InputId, color: Rgb8 }, // inputs « couleur »
    // Lecture (inputs vidéo)
    Play { input: InputId }, Pause { input: InputId }, Restart { input: InputId },
    SetLoop { input: InputId, enabled: bool },
    SetPosition { input: InputId, ms: u64 },
    SetAutoPlay(AutoPlay),                    // { play_on_transition, restart_on_transition, pause_after_transition }
}

pub enum InputSpec {
    Capture { device: DeviceId },             // identifiant stable fourni par io-gst
    File { path: PathBuf },                   // lu en boucle
    Image { path: PathBuf },
    Color { color: Rgb8 },
    TestSource { rate: FrameRate, resolution: Resolution }, // tests, soaks (motif : ajouté si io-gst en a besoin)
}

pub struct CommandSender { /* clonable */ }
impl CommandSender {
    /// Jamais bloquant. Erreur si le canal est plein ou le moteur arrêté.
    pub fn send(&self, source: Source, command: Command) -> Result<CommandId, SendError>;
}

pub struct StateReader { /* ArcSwap<State> */ }
impl StateReader { pub fn load(&self) -> Arc<State>; }
```
Tous les types de commande et d'état sont `serde::Serialize + Deserialize` (journal, futur
API native).

## État (`State`)
| Champ | Contenu |
|---|---|
| `version` | `u64`, +1 à chaque changement |
| `format` | cadence maître, résolution (fixées au démarrage en Phase 1) |
| `inputs` | liste ordonnée : `id` (jamais réutilisé), `number` (1…n, ordre d'affichage), `name`, `spec`, `status` |
| `preview`, `program` | `Option<InputId>` |
| `transition` | `None`, `Timed { from, to, effect, frames, started_at: Option<u64> }` ou `Fader { from, to, value }` (T-Bar) |
| `buttons` | 4 × `(effect, duration_ms)` ; défaut (à confirmer, `phase-1.md` §7.3) : Fade 500, Fade 1000, Fade 1500, Cut |
| `quick_play` | `(effect, duration_ms)`, défaut Fade 500 ms (documenté) |
| `auto_play` | démarrer à l'antenne : oui ; redémarrer : non ; pause en sortie d'antenne : oui (à confirmer) |
| `mix_domain` | `Gamma` (défaut) / `Linear` |

Par input vidéo : `playing`, `looping` (défaut : non), position. `status` d'un input : `Starting` → `Live { resolution, rate, format }` → `SignalLost { since,
attempts }` → (reconnexion) `Starting`…

## Comportement
- Les commandes sont appliquées **dans l'ordre d'arrivée**, une par une, sur `vma-control`.
- **Commande invalide** (input inconnu, durée hors bornes) : refusée, état inchangé, résultat
  `Rejected { reason }` journalisé ; jamais de panique.
- `AddInput` : l'input apparaît immédiatement en `Starting` ; s'il n'y a pas encore de Preview
  ou de Program, il les occupe dans cet ordre : Program d'abord, puis Preview.
- `RemoveInput` d'un input à l'antenne : le Program passe au noir (`program = None`) ; d'un
  input en Preview : `preview = None`. Un Fade qui l'implique est terminé (Cut).
- `Cut` : `program ↔ preview` ; effectif au tick suivant. Sans Preview : refusé.
- `CutDirect { input }` : `program = input`, Preview inchangée ; effectif au tick suivant.
- `Transition { button }` / `QuickPlay { input: None }` : transition Preview → Program avec
  l'effet du bouton (`Cut` = comme `Cut`). Fade : `frames = round(duration_ms × cadence /
  1000)`, au moins 1 ; `duration_ms` ∈ [0, 10 000] (0 = Cut). Progression au tick *k* depuis le
  début : `(k + 1) / frames`, linéaire. À la fin : échange Preview ↔ Program.
- `QuickPlay { input: Some(i) }` : transition de `program` vers `i` ; à la fin, `program = i`
  et la Preview est inchangée (comme `CutDirect`, mais en fondu).
- `SetFader { value }` : démarre (si besoin) une transition manuelle avec l'effet du bouton 1 ;
  progression = `value / 255`. `255` termine : échange, et la T-Bar repart de 0 sans effet
  visible. Pendant une transition minutée, `SetFader` est refusé.
- Transition demandée pendant une transition : refusée ; `Cut` pendant une transition :
  **refusé aussi, la transition continue** (observé sur la référence le 2026-10-26).
- `SetPreview` / `CutDirect` pendant une transition : refusés.
- **Auto-lecture** (inputs vidéo) : au début d'une transition, l'input entrant démarre si
  `play_on_transition` (depuis le début si `restart_on_transition`) ; à la fin, l'input sortant
  est mis en pause si `pause_after_transition`.
- Un input vidéo arrivé à la fin sans Loop reste sur sa dernière image (pas de noir).
- Input en `SignalLost` à l'antenne : il reste sélectionné et le rendu affiche du noir à sa
  place (*question 3*) ; il revient seul dès que le signal revient.

## Journal (JSON Lines)
Fichier `logs/commands-AAAA-MM-JJ.jsonl` (répertoire configurable `--journal-dir`), une ligne
par commande :
```json
{"utc":"2026-10-27T14:03:12.481Z","frame":5321,"source":"ui","id":17,"command":{"Transition":{"button":1}},"result":"Ok","version":42}
```
Écrit par un thread dédié via un canal borné ; si le canal déborde, la ligne est perdue et un
compteur `journal_dropped` augmente (le temps réel prime).

## Mise en œuvre (étape 2 du plan, 2026-10-26)
Modules de `crates/engine` : `command` (vocabulaire), `state` (règles pures, `State::apply` /
`apply_event`), `plan` (`RenderPlan` + seqlock), `sequencer` (côté rendu : plan + index d'image
→ `Composition`, retour `RenderFeedback`), `control` (thread `vma-control`), `journal`.
Précisions apportées en codant (à relire) :
- **Événements** : le rendu signale « transition commencée » (avec l'index de sa première image)
  et « transition terminée » ; `vma-control` en fait des `Event` appliqués à l'état et
  journalisés (`"source":"render"`, champ `event` au lieu de `command` / `result`). C'est ce qui
  rend le **rejeu** exact : `replay(état initial, journal)` réapplique commandes et événements
  et vérifie, ligne par ligne, le même résultat et la même version.
- `Restart` et `SetPosition` ne changent pas l'état (pas de nouvelle version) : ce sont des
  demandes pour `io-gst` (`IoRequest::Seek`). Toute commande acceptée renvoie ses `IoRequest`
  (ouvrir, fermer, lecture, pause, seek, boucle) ; `vma-control` les journalise en `debug`
  jusqu'à l'arrivée d'`io-gst` (étape 7).
- `AddInput` pendant une transition : l'input n'occupe ni Program ni Preview.
- `RemoveInput` d'un input impliqué dans une transition : la transition saute à sa fin, puis
  l'input est retiré.
- T-Bar : revenir à 0 pendant une transition manuelle l'**abandonne** (Program seul, Preview
  inchangée). Avec un effet Cut sur le bouton 1, la bascule se fait à mi-course (128).
- `QuickPlay { input }` sur l'input déjà au Program : refusé. `CutDirect` sur cet input :
  accepté sans effet.
- `PreviewNext` / `PreviewPrevious` : ordre d'affichage, en boucle ; sans Preview, premier /
  dernier input. Refusés pendant une transition, comme `SetPreview`.
- Numéro d'input calculé (position + 1), pas stocké ; `State::fader()` donne la position de la
  T-Bar (0 hors transition manuelle).
- Les raisons de refus sont en anglais (journal, comme les autres messages d'erreur du code) ;
  l'UI les traduira si elle les affiche.
- `RenderFeedback::on_air()` : noir, un input, ou mélange : ce que le dernier tick a montré.
- Ordre de publication : plan d'abord, snapshot ensuite (le rendu n'est jamais en retard sur
  l'UI).

## Critères d'acceptation
| Critère | Test |
|---|---|
| Cut effectif au tick suivant (A3) | horloge simulée : commande entre les ticks *n* et *n+1* → Program changé au tick *n+1* |
| Fade : nombre d'images exact à chaque cadence maître, progression linéaire, swap final sans image intermédiaire (A4) | temps simulé, 6 cadences × durées 0, 1, 40, 500, 1000 ms |
| Quick Play (global, par input), `CutDirect`, 4 boutons, T-Bar (A15–A18) | temps simulé, un test par règle ci-dessus |
| Auto-lecture appliquée aux transitions (A20) | source vidéo factice |
| Commandes invalides refusées sans effet | un test par cas ci-dessus |
| Ordre et déterminisme : rejouer un journal redonne le même état final | test de rejeu |
| Ajout / retrait à chaud sans image manquée (A12) | GPU logiciel, 20 cycles pendant le rendu |
| `vma-control` n'alloue pas sur le thread de rendu : ressources libérées côté contrôle | test : compteur de libérations par thread (instrumentation de `InputResources::drop`) |
| Chaque action UI émet la bonne `Command` (A11) | voir [`ui.md`](ui.md) |
