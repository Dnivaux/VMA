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
    TestSource { rate: FrameRate, resolution: Resolution }, // tests, soaks : la mire du compositeur (générée, pas de pipeline)
    Synthetic { rate: FrameRate, resolution: Resolution },  // soaks, cadences : videotestsrc live (étape 9), tout le chemin d'un input
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

Par input vidéo : `playing`, `looping` (défaut : non). La position et la durée ne sont **pas**
dans l'état versionné (elles changent en continu) : `vma-io` les publie 4 fois par seconde dans
`Timelines` (`RenderEngine::timelines()`, comme les statistiques), avec `ended` (fin atteinte
sans Loop). `status` d'un input : `Starting` → `Live { resolution, rate, format }` → `SignalLost { since,
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
  *(Étape 6)* `Event::InputLive { input, source }` : un feed d'images est attaché à l'input
  (`Starting` → `Live { source: Some(format) }`), journalisé comme les autres événements.
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

## Phase 2 — Sorties (étape 5, 2026-10-31)
Commandes de `phase-2.md` § 5, mises en œuvre telles quelles ; précisions apportées en codant
(à relire) :
- **Réglages** (`OutputSpec`) : `Record { folder, codec, container, video_kbps }`,
  `Rtmp { url, key, video_kbps }`, `Srt { mode, host, port, latency_ms, passphrase,
  video_kbps }`. `folder: None` = dossier Vidéos de l'utilisateur ; `video_kbps: None` = débit
  par défaut de `phase-2.md` § 6. Validés à l'ajout et à la modification (débit 100 à
  200 000 kb/s, adresse `rtmp://` ou `rtmps://`, port non nul, latence SRT 20 à 8 000 ms,
  phrase de passe vide ou de 10 à 79 caractères) : sinon refus, état inchangé.
- **Un enregistrement existe dès le départ** (`State::DEFAULT_RECORDING`, `output#1`), pour que
  le bouton Enregistrer marche tout de suite. Il peut être modifié ou supprimé
  (`StartRecording` est alors refusé : « no recording output is configured »).
- **Destinations de stream** : 3 au plus, **RTMP ou SRT chacune** ; une destination ajoutée
  prend le plus petit numéro libre (1 à 3), affiché « Stream n ». `StartStreaming { slot }`
  agit sur la destination `slot`, ou sur toutes (`None`). 8 sorties au plus en tout.
- **État d'une sortie** : `Idle → Starting → Running { encoder, target } → Stopping → Idle`, ou
  `Failed { reason }` ; `Reconnecting { attempt }` est réservé aux streams (étape 6). Chaque
  démarrage reçoit un **numéro de run** : `io-gst` le rappelle dans ses événements
  (`OutputRunning`, `OutputStopped`, `OutputFailed`, journalisés comme les autres), et ceux d'un
  run ancien sont ignorés (un arrêt suivi d'un démarrage rapide ne se mélange pas).
- Démarrer une sortie qui tourne : sans effet ; en cours d'arrêt : refusé. Arrêter une sortie en
  erreur la remet à l'arrêt (acquittement) ; une confirmation de démarrage arrivée après un
  arrêt la laisse en cours d'arrêt. Modifier une sortie active : refusé ; changer un
  enregistrement en stream (ou l'inverse) : refusé. Supprimer une sortie active l'arrête.
- **`Secret`** (clé de stream, phrase de passe SRT) : `Serialize` et `Debug` donnent `***`
  (vide reste vide : rien de secret). Le journal, les logs, l'état sérialisé et les rapports
  n'en contiennent donc jamais la valeur (B13). Relu depuis un journal, il vaut
  `Secret::Redacted` : le démarrage de la sortie est refusé (« the secret of output#n is
  missing… »), le rejeu s'arrête là avec cette raison, et la clé doit être saisie de nouveau.
- **FTB** : `FadeToBlack` bascule `ftb.active` ; la durée (`SetFadeToBlackDuration`, 0 à
  10 000 ms, défaut 1 000 ms) donne `round(durée × cadence)` ticks, 0 = immédiat. Le fondu
  lui-même est calculé par le rendu (comme une transition) : le *k*-ième tick après la
  commande montre `(k + 1) / images`. **Relancé en cours de fondu, il repart d'où il en est, à
  la même vitesse** (retour en 10 ticks après 10 ticks d'un fondu de 50) : choix à confirmer
  sur la référence (question 16 de `observation-reference-phase2.md`). Le niveau n'est pas
  dans l'état versionné : `RenderFeedback::fade_to_black()` (0 = Program, 1 = noir).
- Requêtes vers `vma-io` : `IoRequest::StartOutput { output, run, spec }` et
  `StopOutput { output }`.
- *(Étape 6)* `Reconnecting { attempt, reason }` (raison de la dernière perte) sur
  `Event::OutputReconnecting` : seulement depuis une sortie qui produit (démarrage, en cours, en
  reconnexion) ; une perte arrivée après un arrêt est ignorée. Une sortie en reconnexion compte
  comme active (réglages figés, boutons « en direct »).

- *(Étape 8)* **Plein écran** : `OutputSpec::Fullscreen { screen: Option<String> }`, le nom de
  l'écran tel que le système le donne (« DP-2 », « HDMI-A-1 »), `None` = le premier de la
  liste ; nom vide refusé. **Une seule** sortie plein écran (`FullscreenExists` : un seul
  lecteur de ses images) ; ce n'est pas une destination de stream (pas de numéro). Pas de
  pipeline ni d'encodeur : `StartOutput` la met **tout de suite** en `Running { encoder: "",
  target: Some(écran) }` sans requête à `vma-io`, `StopOutput` la remet tout de suite à
  `Idle` ; `StartRecording` / `StartStreaming` ne la touchent pas. Elle ne change pas de type
  (`OutputKindChanged`). Le plan du rendu porte `fullscreen` (une sortie plein écran en
  marche) : le rendu ne remplit ses images que dans ce cas. Un écran débranché laisse la
  sortie en marche, non affichée (côté UI, voir `ui.md`).

- *(Étape 9)* **Perte du GPU** (ADR 0006 § 7) : `vma-control` lit
  `RenderFeedback::device_lost_at()` à chaque tour. `Event::DeviceLost { frame }` (une fois,
  journalisé) remplit `State::device_lost` : les sorties continuent (le rendu leur envoie du
  noir), tout démarrage est refusé (`Rejection::DeviceLost`, « the GPU is lost: no output can
  start »), arrêter reste possible. `EngineConfig::device_lost_grace` (10 s) après l'avoir vu,
  `Event::DeviceLostGraceOver` (une fois, journalisé) arrête toutes les sorties qui produisent
  (`StopOutput` vers `vma-io`, plein écran tout de suite à l'arrêt) ; une sortie en erreur
  garde sa raison. Sans perte, cet événement est sans effet.

## Phase 3 — Audio (étape 4, 2026-11-03)
Commandes de `phase-3.md` § 5, mises en œuvre telles quelles ; précisions apportées en codant
(à relire) :
- **Niveaux** : type `Db` (`f32` fini ou −∞, jamais NaN : la commande reste comparable) ; le
  journal écrit −∞ `"-inf"` (JSON n'a pas d'infini), un nombre sinon
  (`{"SetInputGain":{"input":2,"db":-6.5}}`). Fader d'input et de bus : −∞ à +10 dB, au-delà
  refusé (`GainOutOfRange`) ; décalage 0 à 1 000 ms (`DelayOutOfRange`).
- **Inputs avec son** (`InputSpec::has_audio`) : capture, fichier vidéo, source de synchro
  (NDI, SRT et périphérique audio s'ajouteront). Couleur, image, mire et `Synthetic`
  (`videotestsrc`) n'en ont pas : leurs commandes audio sont refusées (`NoAudio`). Une capture
  sans son garde une tranche muette.
- **État** : `InputState::audio: Option<InputAudio>` (fader, mute, solo, décalage, envois
  Master / A / B, audio-follow-video), défauts de `phase-3.md` § 6 à l'ajout ;
  `State::audio` (`AudioState`) : les 3 bus (fader, mute) et le casque (`Monitor` :
  périphérique, `SoloMode` AFL / PFL) ; `OutputState::bus` (Master par défaut). Champs relus
  avec leur défaut (`serde(default)`) dans un journal ou un état d'avant la Phase 3.
- **`SetOutputBus`** : refusé pendant que la sortie tourne (`OutputActive`) et pour le plein
  écran (`OutputWithoutSound`) ; `IoRequest::StartOutput` porte le bus.
- **`SetMonitor`** : périphérique (nom vide refusé) et mode de solo. Le mode agit dès
  maintenant sur le mix casque et son vumètre ; le périphérique n'est joué qu'à l'étape 5.
- *(Étape 5)* **Input son seul** : `InputSpec::AudioDevice { device }` (identifiant de
  `io-gst`), nommé par l'UI d'après le périphérique (« Son » sinon). Muet et sans
  audio-follow-video à l'ajout ; il **n'occupe ni le Program ni la Preview** (pas d'image) ;
  `Event::AudioLive { input }` (son qui arrive) le met en `Live { source: None }`, journalisé
  comme les autres événements. `SetMonitor` envoie `IoRequest::SetMonitor { device }` à
  `vma-io` quand le périphérique change (pas pour le seul mode de solo).
- *(Étape 6)* **Input SRT** : `InputSpec::Srt(SrtSource { mode, host, port, latency_ms,
  passphrase })`, validé à l'ajout comme une sortie SRT (`Rejection::InvalidInput`) ; la
  phrase de passe est un `Secret` (`***` partout). Relue d'un journal, elle manque : l'input
  est perdu avec la raison (« add the input again »).
- **Plan audio** : `State::audio_plan()` (les inputs avec son dans l'ordre d'affichage, les
  bus, le mode de solo), publié par `vma-control` après chaque changement par un seqlock
  d'atomiques (`audio_plan.rs`, 133 mots ; le seqlock du plan du rendu est devenu générique,
  `SeqLock<N>`), lu par `vma-audio` à chaque bloc. Un input absent du plan (retiré) n'est
  plus mixé.

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
| *(Phase 2)* Sorties : démarrage, arrêt, runs, destinations 1–3, refus sans effet | `state::tests` (8 tests) |
| *(Phase 2)* **B11** côté moteur : FTB de la durée réglée aux 6 cadences, retour, Program inchangé | `control::tests::fade_to_black_*` (temps simulé), `render_loop::fade_to_black_darkens_the_outputs_not_the_program_view` (GPU) |
| *(Phase 2)* **B13** côté moteur : ni la clé ni la phrase de passe dans le journal, les logs (niveau TRACE) ou l'état sérialisé ; rejeu refusé sans la clé | `control::tests::secrets_never_reach_the_journal_the_logs_nor_the_published_state` |
| *(Phase 2, étape 9)* **B14** côté état et contrôle : perte journalisée une fois, démarrages refusés, sorties qui produisent finalisées au bout du délai (pas avant, une fois), sorties en erreur ou à l'arrêt intactes | `state::tests::a_lost_gpu_refuses_starts_then_finalises_what_runs`, `control::tests::a_lost_gpu_is_journaled_and_finalises_the_outputs_after_the_grace` (temps simulé) |
| *(Phase 2, étape 8)* Plein écran : en marche sans `vma-io`, une seule, réglages figés en marche, type figé, arrêt et suppression sans requête, drapeau du plan | `state::tests::the_full_screen_runs_without_io_and_sets_the_plan`, `plan::tests::encoding_round_trips` |
| *(Phase 3, étape 4)* Audio : défauts par type d'input, chaque commande, refus sans effet (input sans son, inconnu, hors bornes), bus des sorties (figé en marche, refusé au plein écran) | `state::tests::{inputs_with_sound_get_their_default_settings, audio_commands_change_the_settings, audio_commands_are_refused_without_effect, outputs_carry_their_bus}` |
| *(Phase 3, étape 6)* Input SRT : réglages validés, phrase de passe jamais écrite | `state::tests::srt_inputs_are_checked`, `control::tests::secrets_never_reach_the_journal_the_logs_nor_the_published_state` |
| *(Phase 3, étape 5)* Input son seul (défauts, ni Program ni Preview, `AudioLive` une fois) ; casque : `SetMonitor` vers `vma-io` au changement de périphérique seulement | `state::tests::{a_sound_only_input, the_headphones_device_goes_to_io}` |
| *(Phase 3, étape 4)* **C13** côté moteur : toute commande audio journalisée (−∞ compris), rejeu → même état, plan audio publié | `control::tests::audio_settings_are_journaled_replayed_and_published`, `command::tests::levels_in_the_journal`, `audio_plan::tests` (aller-retour, lectures concurrentes jamais déchirées) |
