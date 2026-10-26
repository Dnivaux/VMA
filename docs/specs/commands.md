# Spec — couche de commandes et état (`engine`, Phase 1)

- **Statut** : proposé, à valider avec [`phase-1.md`](phase-1.md) et l'ADR 0003

## Interface publique (esquisse)
```rust
pub enum Command {
    AddInput { spec: InputSpec, name: Option<String> },
    RemoveInput { input: InputId },
    SetPreview { input: InputId },
    /// Envoie directement un input au Program (sans transition).
    SetProgram { input: InputId },
    Cut,
    /// `None` = durée courante du Fade.
    Fade { duration_ms: Option<u32> },
    SetFadeDuration { duration_ms: u32 },
    SetMixDomain { domain: MixDomain },      // Gamma (défaut) | Linear
    SetColor { input: InputId, color: Rgb8 }, // inputs « couleur »
}

pub enum InputSpec {
    Capture { device: DeviceId },             // identifiant stable fourni par io-gst
    File { path: PathBuf },                   // lu en boucle
    Image { path: PathBuf },
    Color { color: Rgb8 },
    TestSource { pattern: TestPattern, rate: FrameRate, resolution: Resolution }, // tests, soaks
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
| `transition` | `None` ou `Fade { from, to, frames, started_at: Option<u64> }` |
| `fade_duration_ms` | défaut 1000 |
| `mix_domain` | `Gamma` (défaut) / `Linear` |

`status` d'un input : `Starting` → `Live { resolution, rate, format }` → `SignalLost { since,
attempts }` → (reconnexion) `Starting`…

## Comportement
- Les commandes sont appliquées **dans l'ordre d'arrivée**, une par une, sur `vma-control`.
- **Commande invalide** (input inconnu, durée hors bornes) : refusée, état inchangé, résultat
  `Rejected { reason }` journalisé ; jamais de panique.
- `AddInput` : l'input apparaît immédiatement en `Starting` ; s'il n'y a pas encore de Preview
  ou de Program, il les occupe dans cet ordre : Program d'abord, puis Preview.
- `RemoveInput` d'un input à l'antenne : le Program passe au noir (`program = None`) ; d'un
  input en Preview : `preview = None`. Un Fade qui l'implique est terminé (Cut).
- `Cut` : `program ↔ preview` (*question 1* de `phase-1.md`) ; effectif au tick suivant.
- `Fade` : `frames = round(duration_ms × cadence / 1000)`, au moins 1 ; `duration_ms` ∈
  [0, 10 000] (0 = Cut). Progression au tick *k* depuis le début : `(k + 1) / frames`, linéaire.
  À la fin : même échange que `Cut`.
- `Fade` pendant un Fade : ignoré (`Rejected`) ; `Cut` pendant un Fade : le termine
  immédiatement (*question 2*).
- `SetPreview` / `SetProgram` pendant un Fade : refusés (le Fade doit finir ou être coupé).
- Input en `SignalLost` à l'antenne : il reste sélectionné et le rendu affiche du noir à sa
  place (*question 3*) ; il revient seul dès que le signal revient.

## Journal (JSON Lines)
Fichier `logs/commands-AAAA-MM-JJ.jsonl` (répertoire configurable `--journal-dir`), une ligne
par commande :
```json
{"utc":"2026-10-27T14:03:12.481Z","frame":5321,"source":"ui","id":17,"command":{"Fade":{"duration_ms":null}},"result":"Ok","version":42}
```
Écrit par un thread dédié via un canal borné ; si le canal déborde, la ligne est perdue et un
compteur `journal_dropped` augmente (le temps réel prime).

## Critères d'acceptation
| Critère | Test |
|---|---|
| Cut effectif au tick suivant (A3) | horloge simulée : commande entre les ticks *n* et *n+1* → Program changé au tick *n+1* |
| Fade : nombre d'images exact à chaque cadence maître, progression linéaire, swap final sans image intermédiaire (A4) | temps simulé, 6 cadences × durées 0, 1, 40, 500, 1000 ms |
| Commandes invalides refusées sans effet | un test par cas ci-dessus |
| Ordre et déterminisme : rejouer un journal redonne le même état final | test de rejeu |
| Ajout / retrait à chaud sans image manquée (A12) | GPU logiciel, 20 cycles pendant le rendu |
| `vma-control` n'alloue pas sur le thread de rendu : ressources libérées côté contrôle | test : compteur de libérations par thread (instrumentation de `InputResources::drop`) |
| Chaque action UI émet la bonne `Command` (A11) | voir [`ui.md`](ui.md) |
