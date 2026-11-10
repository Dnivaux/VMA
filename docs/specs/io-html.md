# Spec — `io-html` : input HTML (CEF hors écran)

- **Statut** : écrit à l'étape 5 de la Phase 4 (2026-11-10), d'après l'ADR 0009 (accepté) et
  l'essai CEF de l'étape 4 ; les deux écarts à l'ADR (§ 3) sont retenus (2026-11-10)
- **Références** : [`phase-4.md`](phase-4.md) (D4 à D7), ADR 0009 (input HTML), ADR 0010
  (alpha), [`io-gst.md`](io-gst.md) (feeds), [`commands.md`](commands.md) (`InputSpec::Html`)

## 1. Rôle
Rendre une page web hors écran, avec son alpha, et en faire un input comme les autres : ses
images vont dans le feed de l'input (ADR 0004), le rendu ne sait pas qu'il s'agit de HTML.
Chromium tourne dans un **processus à part, `vma-html`** : `vma` ne se lie jamais à CEF,
démarre sans lui, et un plantage de Chromium ne fait tomber que les inputs HTML.

```
 vma (application)                                   vma-html (CEF, processus à part)
 ┌────────────────────────────────────────┐          ┌──────────────────────────────────┐
 │ vma-io : HtmlHost (open / close / tick)│─TCP────► │ thread UI de CEF : un navigateur │
 │   lance, surveille, relance vma-html   │ 127.0.0.1│ hors écran par input             │
 │                                        │ JSON/ligne│  BeginFrame à la cadence        │
 │ thread vma-html : Frame → feed, Release│◄─────────│  OnPaint → slot libre → Frame    │
 │        ▲ lit le slot                   │          │        │ écrit le slot           │
 └────────┼───────────────────────────────┘          └────────┼─────────────────────────┘
          └──────── mémoire partagée : 3 images BGRA par input ┘
```

## 2. Composants
- **Bibliothèque `vma-io-html`** (sans CEF) : `protocol` (messages, ligne de commande de
  `vma-html`), `shm` (images partagées), `HtmlHost` (côté application). Testée en CI ordinaire
  contre un faux `vma-html` (un thread qui parle le protocole).
- **Binaire `vma-html`** (feature `cef`, hors des builds ordinaires) : mode service
  (`--vma-connect=ADRESSE --vma-token=JETON`, lancé par `HtmlHost`) et sonde de l'étape 4
  (`--vma-probe`). Il est aussi l'exécutable des sous-processus de Chromium (`--type=…`).
- **`vma-io`** (`io-gst`) : tient un `HtmlHost` ; `InputSpec::Html` → `open`, retrait → `close` ;
  l'adaptateur crée les feeds en **BGRA, alpha prémultiplié, cadence du maître**.
- **Trouver `vma-html`** : la variable `VMA_HTML`, sinon à côté de l'exécutable de `vma`. Absent
  → `HtmlStatus::Unavailable` avec la raison (`IoRuntime::html`), comme NDI® : les inputs HTML
  sont perdus avec cette raison, rien n'est lancé.

## 3. Protocole (`vma_io_html::protocol`)
- **Canal** : une connexion TCP locale. `vma` écoute sur `127.0.0.1` (port libre), lance
  `vma-html` avec l'adresse et un **jeton** ; `vma-html` se connecte et envoie
  `Hello { version, token }`. Mauvais jeton ou autre version → refusé.
  *Écart à l'ADR 0009 § 3* (qui dit « pipe des entrées / sorties standard ») : les
  sous-processus de Chromium héritent de la sortie standard, et tout ce qu'ils y écriraient
  casserait le découpage des messages.
- **Messages** : un objet JSON par ligne (JSON échappe les retours à la ligne des chaînes), au
  plus 1 Mio.
  - `vma` → `vma-html` : `Create { browser, url, width, height, rate_num, rate_den, frames }`,
    `Reload`, `Close`, `Release { browser, slot }`, `Quit`.
  - `vma-html` → `vma` : `Hello`, `Loaded { http_status }`, `Frame { slot, seq }`,
    `RendererGone { reason }`, `Failed { reason }`, `Closed`, **`Alive`** (toutes les 250 ms).
  *Ajout à l'ADR 0009* : `Alive`. Une page statique ne peint que quand elle change, une page
  tombée plus du tout : sans `Alive`, `vma` prendrait un `vma-html` en bonne santé pour figé
  (vu au test).
- **Images partagées** (`vma_io_html::shm`) : un fichier projeté en mémoire par input (dans
  `/dev/shm` sous Linux, le dossier temporaire ailleurs), **3 emplacements** d'une image BGRA
  `width × height × 4`. Créé par `vma`, ouvert par `vma-html`, supprimé à la fermeture de
  l'input (sous Linux dès la première image : rien ne reste si `vma` plante).
- **Propriété des emplacements** : `vma-html` écrit un emplacement libre puis envoie `Frame` ;
  l'emplacement est à `vma` jusqu'à son `Release`. Jamais écrit et lu en même temps. Une image
  qui ne trouve pas d'emplacement libre est jetée et comptée côté `vma-html` (`vma` en
  retard).

## 4. Cadence (étape 6)
Une image de la page par tick du maître (ADR 0009 § 4) :
1. le thread de rendu, au réveil de chaque tick *n* et avant tout le reste, fait un `try_send`
   de *n* sur `IoPort::html_ticks` (4 places) ; plein → tick jeté et compté
   (`RenderStatsSnapshot::html_ticks_dropped`), jamais d'attente ;
2. le thread `vma-html-ticks` de l'application (dans `HtmlHost`) l'envoie à `vma-html`
   (`BeginFrame { tick }`) ; en retard, il n'envoie que le dernier tick reçu ;
3. `vma-html` fait un `SendExternalBeginFrame` par navigateur (plusieurs `BeginFrame` en
   attente comptent pour un ; ceux d'avant l'initialisation de CEF sont ignorés) : la page
   avance d'une étape, peint, l'image part dans le feed ;
4. elle est montrée au tick *n + 1* : latence d'une période (mesurée : 18,7 ms à 50 i/s).
Les navigateurs ne peignent que sur ces `BeginFrame` (plus d'horloge propre à `vma-html`).
Options de Chromium fixées par `vma-html` (essai de l'étape 4) : `--ozone-platform=headless`
(Linux), `--run-all-compositor-stages-before-draw`, `--disable-gpu`.

## 4 bis. Commandes de la page (étape 7)
Alignées sur l'input navigateur de la référence (documentation publique : adresse, largeur,
hauteur, **CSS** appliquée au chargement ; changer d'adresse, recharger, page précédente /
suivante), plus les **données** que le BRIEF demande (mise à jour sans rechargement, D6) :

| Commande (`commands.md`) | Message à `vma-html` | Dans la page |
|---|---|---|
| `HtmlData { json }` | `Script` : `window.dispatchEvent(new CustomEvent("vma-data", { detail: (json) }))` | `addEventListener("vma-data", e => … e.detail …)` ; pas de rechargement |
| `HtmlScript { script }` | `Script` | exécuté tel quel dans le cadre principal |
| `HtmlReload` | `Reload` | rechargée (sans cache) |
| `SetHtmlUrl { url }` | `Navigate` | autre page ; l'adresse est gardée pour les ouvertures suivantes |
| `HtmlBack` / `HtmlForward` | `Back` / `Forward` | historique du navigateur |
| `InputSpec::Html { css }` | `Create { css }` | une balise `<style>` ajoutée à **chaque** page chargée (à la fin du chargement : la page peut se montrer un instant sans elle) |

Une action pour une page pas encore ouverte (`vma-html` absent, en relance) est perdue, sauf
l'adresse. Chez la référence, une page reçoit ses données en interrogeant l'API du mélangeur :
chez VMA ce sera aussi possible avec l'API de la Phase 5.

**UI** : « Ajouter → Page web (HTML)… » (adresse, taille du Program ou taille propre, CSS),
grisé avec la raison sans `vma-html` (D7) ; réglages de l'input HTML : adresse (identifiants
masqués) et « Aller », « Recharger », « Précédente », « Suivante », données JSON et
« Envoyer les données », script et « Exécuter le script ».

**Différences avec la référence, reportées** : cliquer et taper dans la page (souris et
clavier transmis à CEF) ; une page plus petite que le Program est centrée à sa taille chez la
référence, mise à l'échelle chez VMA (placement d'un input : « position / zoom », hors Phase 4).

## 4 ter. Son de la page (étape 9, Q9)
- `vma-html` demande à CEF le son de chaque page en 48 kHz stéréo, par paquets de 10 ms
  (`AudioHandler` : il n'est plus joué sur la machine), et autorise la lecture sans geste
  (`--autoplay-policy=no-user-gesture-required` : personne ne clique sur un overlay).
- CEF appelle ce gestionnaire sur **un thread à lui**, pas le thread UI : il ne touche à rien
  du service (`Rc`, `RefCell`) ; ses paquets, entrelacés en stéréo (mono des deux côtés, les
  deux premiers canaux au-delà), passent par un canal au thread principal, seul à écrire sur la
  connexion (`Audio { browser, rate, samples }`, échantillons F32 en base64). Vu au test : y
  toucher depuis ce thread était une course de données.
- **Daté à l'arrivée** : images et son de l'input HTML portent l'heure de l'horloge maître à
  leur réception (un paquet : celle de sa fin moins sa durée) ; `vma-audio` lit le son à l'heure
  de l'image montrée, comme pour tout input. Un son à une autre cadence que 48 kHz est ignoré
  (signalé une fois).
- L'input HTML a une tranche dans le mélangeur ; comme toute source avec image, il suit la
  vidéo par défaut (au Master quand il est au Program ; en overlay, seulement si on désactive
  le suivi, q. 26 de l'observation).

## 5. Pannes (ADR 0009 § 6)
Chacune met l'input « signal perdu » avec sa raison ; il repasse en direct avec un **nouveau
feed** à la première image qui suit.

| Panne | Réaction |
|---|---|
| `vma-html` introuvable | inputs HTML perdus avec la raison, rien de lancé |
| Processus de rendu de la page terminé (`RendererGone`), page non chargée (`Failed`) | cet input perdu ; `Reload` après 1 s, au plus une fois toutes les 5 s ; les autres pages continuent |
| `vma-html` arrêté, connexion fermée, rien reçu depuis 2 s (pas même `Alive`), pas de `Hello` sous 10 s | tous les inputs HTML perdus, `vma-html` tué et relancé, au plus une fois toutes les 5 s ; les navigateurs sont recréés |
| Autre version de `vma-html` | inputs perdus (« d'une autre version ») |
| Plus d'input HTML | `Quit`, `vma-html` tué au bout de 2 s s'il tourne encore |

Rien de cela ne passe par le thread de rendu : `HtmlHost` tourne sur `vma-io`, les images sur
le thread `vma-html` de l'application (copie de l'emplacement dans le feed, qui ne bloque
jamais).

## 6. Critères (étape 5)
| Critère | Test | Où |
|---|---|---|
| Messages : aller-retour, une ligne par message, ligne tronquée ou sans fin refusée ; ligne de commande écrite = lue | `protocol::tests` (4) | CI |
| Images partagées : ce qu'un côté écrit, l'autre le lit ; taille vérifiée ; fichier supprimé | `shm::tests` (3) | CI |
| **D7** côté application (faux `vma-html`) : chaque page dans son feed, dans l'ordre ; absent → refus avec raison ; page tombée → perdue, rechargée après le délai, nouveau feed, autres pages intactes ; rechargements espacés ; `vma-html` mort → tout perdu puis relancé ; figé → tué et relancé ; autre version refusée ; page muette (statique ou tombée) sans relance ; dernier input fermé → `vma-html` arrêté ; images partagées supprimées | `host::tests` (10) | CI |
| **D4** avec le vrai `vma-html` : page à fond transparent → feed en 1080p (taille du Program), BGRA prémultiplié (128, 0, 0, 128 pour `rgba(255, 0, 0, 0.5)`), fond (0, 0, 0, 0), ≈ 50 images/s | `tests/real_helper.rs` | job CI `html` (2 OS) |
| **D7** avec le vrai `vma-html` (Linux) : processus de rendu tué → perdu, rechargé, nouveau feed ; `vma-html` tué → relancé, en direct | `tests/real_helper.rs` | job CI `html` (Linux) |
| **D4 / D7** avec le moteur réel : la page en overlay sur le Program, incrustée avec son alpha (128, 0, 127 sur du bleu), bleu où la page est vide ; `vma-html` tué → perdu, de nouveau en direct et incrusté, **0 tick manqué** | `io-gst/tests/html.rs` | job CI `html` (Linux, lavapipe) |
| `InputSpec::Html` : schémas, nom, identifiants jamais écrits | voir [`commands.md`](commands.md) | CI |
| *(étape 7)* **D6** avec le vrai `vma-html` : donnée montrée **sous 2 images**, page pas rechargée (compteur de chargements inchangé) ; script exécuté ; `HtmlReload` → rechargée, CSS de nouveau appliquée ; autre adresse puis page précédente | `tests/real_helper.rs::a_page_takes_data_and_scripts_without_reloading` | job CI `html` (2 OS) |
| *(étape 7)* Actions de page à l'hôte : données en événement `vma-data`, adresse gardée pour la relance | `host::tests::page_actions_reach_the_open_page_and_navigate_is_kept` | CI |
| *(étape 7)* **D9** (HTML) : commandes vers `vma-io` pour un input HTML seulement, JSON vérifié, textes ≤ 64 Kio, adresse gardée ; journalisées en entier sauf identifiants, rejeu → même état ; UI (A11) : ajout, grisé, réglages | `state::tests::html_commands_reach_the_page_of_html_inputs_only`, `control::tests::html_commands_are_journaled_and_replayed`, `ui/tests/switcher.rs` (3) | CI |
| *(étape 9)* Son : une page qui joue une sinusoïde à 0,5 en WebAudio (sans clic) → son au feed en 48 kHz stéréo, au rythme du temps (± 10 %), RMS 0,354 | `tests/real_helper.rs::the_sound_of_a_page_reaches_its_feed` | job CI `html` (2 OS) |
| *(étape 9)* Son sur le moteur réel : l'input HTML a une tranche ; au Program, sa page s'entend au Master à son niveau | `io-gst/tests/html.rs::the_sound_of_an_html_page_on_program_reaches_the_master` | job CI `html` (Linux) |
| *(étape 9)* **D12** : CEF et Chromium dans « À propos », avis de Chromium (`CREDITS.html`) à côté de `vma-html` | `ui/tests/switcher.rs::without_the_ndi_runtime_ndi_is_greyed`, job CI `html` | CI |
| *(étape 6)* Les ticks du maître arrivent à `vma-html` en `BeginFrame`, un retard ne passe que le dernier | `host::tests::master_ticks_reach_the_helper_as_begin_frames_latest_first` | CI |
| **D5** (BRIEF « sans saccade ») : page animée (une barre qui avance d'une colonne sur 16 à chaque `requestAnimationFrame`) en overlay, **chaque image du Program** relue sur une sortie pendant 60 s après 3 s de chauffe : chaque image montre l'étape suivante et une seule ; feed de l'input sans image répétée ni sautée ; latence p99 ≤ 2 images ; 0 tick manqué | `io-gst/tests/html.rs::an_animated_page_shows_one_new_step_per_program_picture` (`VMA_D5_SECS`, `VMA_D5_RATE`) | job CI `html` (Linux, lavapipe : 50 sur 60 s, 59,94 sur 20 s) ; machine de dev : 50 / 60 s (3000 / 3000), 25, 29,97, 59,94, 60 / 20 s, 0 fausse |

Le paquet (avec `CREDITS.html` et `libcef` strippée) viendra avec la première version. Jalon visible : `vma --html
page.html`, puis le bouton **1** de sa vignette.
