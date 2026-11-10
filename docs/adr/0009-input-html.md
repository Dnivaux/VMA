# ADR 0009 — Input HTML : CEF hors écran dans un processus auxiliaire

- **Statut** : **accepté** le 2026-11-09 par le mainteneur (avec la spec de la Phase 4) ; mesures de l'essai (étape 4) ajoutées le 2026-11-10
- **Date** : 2026-11-09
- **Contexte** : BRIEF §2.4 (une entrée qui tombe ne fait jamais tomber le programme), §2.6
  (licences), §4 (Phase 4 : page web hors écran avec alpha, 50 fps, données sans rechargement),
  §6 (CEF, feature optionnelle isolée dans `io-html`) ; ADR 0002 (titres HTML), ADR 0004
  (entrées) ; spec [`docs/specs/phase-4.md`](../specs/phase-4.md)

## Faits vérifiés (2026-11-09)
- Crate **`cef`** 156.4.0+156.0.26 (dépôt `tauri-apps/cef-rs`, **Apache-2.0 OR MIT**) et
  `cef-dll-sys` (même licence) : bindings de l'API C de CEF pour Linux, Windows et macOS.
  CEF lui-même est sous **BSD-3-Clause** ; Chromium apporte les licences de ses composants
  (avis à joindre au paquet).
- À la compilation, `cef-dll-sys` **télécharge une distribution de CEF** (builds automatiques
  publiques de CEF ; ou `CEF_PATH` pour un dossier déjà présent) et compile le wrapper C++
  (`cmake`, compilateur C++).
- Sous **Linux et Windows**, le binaire **se lie dynamiquement à `libcef`** (`rustc-link-lib
  =dylib`) : un exécutable qui utilise la crate ne démarre pas sans les bibliothèques de CEF.
  Sous macOS seulement, CEF est chargé à l'exécution.
- Les bindings exposent : rendu hors écran (`windowless_rendering_enabled`, `OnPaint` en
  mémoire BGRA), **`external_begin_frame_enabled`** et **`SendExternalBeginFrame`** (l'hôte
  décide quand la page produit une image), `ExecuteJavaScript`, `OnRenderProcessTerminated`,
  `OnAudioStreamPacket` (son de la page), `shared_texture_enabled` et `OnAcceleratedPaint`.
- La feature `accelerated_osr` de la crate importe la texture partagée de CEF vers une
  texture **wgpu 30** (notre version) : DMA-BUF par Vulkan sous Linux, D3D11 sous Windows. Elle
  demande `unsafe` et les features Vulkan de mémoire externe.
- Les builds publiques de CEF n'ont pas les codecs propriétaires (H.264 / AAC dans une
  `<video>` de page).

## Décision
1. **CEF par la crate `cef`, dans un processus auxiliaire `vma-html`** (binaire du workspace,
   crate `io-html`, hors des `default-members` : un `cargo build` ordinaire ne télécharge pas
   CEF). Le binaire `vma` **ne se lie jamais à CEF**.
2. `vma-io` lance `vma-html` au premier input HTML, l'arrête quand il n'y en a plus. Un seul
   processus pour tous les inputs HTML (un navigateur hors écran par input). `vma-html` est
   aussi l'exécutable des sous-processus de Chromium (rendu, GPU), qui se relance lui-même.
3. **Échanges** :
   - commandes (créer / fermer un navigateur, URL, taille, données, script, rechargement,
     `BeginFrame`) et événements (chargé, image prête, perdu, son) sur un canal local
     (pipe des entrées / sorties standard du processus, messages encadrés) ;
   - **images par mémoire partagée** : un anneau de 3 images BGRA par input (en-tête à
     numéro de séquence, comme le seqlock du plan de rendu), écrit par `OnPaint`, lu par un
     thread de l'application qui téléverse dans le feed de l'input (chemin des entrées
     actuelles, ADR 0004). Le rendu ne voit qu'un feed comme les autres.
4. **Cadence** : `external_begin_frame_enabled`. Le rendu envoie le tick *n* au thread
   `vma-html` de l'application (`try_send`, comme vers `vma-audio`), qui envoie `BeginFrame` à
   chaque navigateur. La page produit au plus une image par tick, son horloge d'animation
   avance d'une période du maître par tick. Pas de minuterie propre à CEF.
5. **Données** : `HtmlData { json }` exécute `window.dispatchEvent(new CustomEvent("vma-data",
   { detail: <json> }))` ; `HtmlScript` exécute le script tel quel ; ni l'un ni l'autre ne
   recharge la page.
6. **Pannes** : `vma-html` introuvable ou qui ne démarre pas → inputs HTML refusés avec raison
   (`IoStatus::html`, comme NDI) ; processus de rendu d'une page terminé → input « signal
   perdu », rechargé après 1 s ; `vma-html` qui meurt ou ne répond plus (pas d'image ni de
   réponse depuis 2 s alors que des `BeginFrame` partent) → tous les inputs HTML perdus,
   processus relancé (au plus une fois toutes les 5 s). Rien de tout cela ne touche le rendu.
7. **Rendu logiciel d'abord** (`OnPaint`, mémoire) ; le partage de texture
   (`OnAcceleratedPaint`, descripteur DMA-BUF ou handle D3D11 passé à l'application, importé
   par `unsafe` justifié) est une optimisation ultérieure, décidée sur mesure.
8. **Bac à sable de Chromium désactivé** en Phase 4 (pages choisies par l'opérateur) ; à
   reprendre avant d'ouvrir à des pages d'Internet non maîtrisées.
9. **Distribution** : `vma-html` et les fichiers de CEF à côté du binaire ; version de CEF
   épinglée par la version de la crate. Avis de licence de CEF et de Chromium dans le paquet et
   dans « À propos ».

## À vérifier (étape 4 de la spec, point d'arrêt si une hypothèse tombe)
- Le rendu hors écran tourne **sans écran** (CI Linux : plateforme *headless* de Chromium ou
  Xvfb ; CI Windows). → **Oui** sous Linux, avec `--ozone-platform=headless` (sans lui,
  Chromium cherche un serveur d'affichage et `CefInitialize` échoue) ; Windows : job CI.
- `OnPaint` donne l'alpha d'une page à fond transparent, et sous quelle forme (prémultiplié
  attendu) ; `AlphaMode` de l'input en conséquence. → **Prémultiplié** : `AlphaMode::Premultiplied`.
- **Une image par `BeginFrame`**, et `requestAnimationFrame` cadencé par eux (D5). → **Oui**, avec
  `--run-all-compositor-stages-before-draw`.
- Coût : CPU, mémoire par navigateur, copie 1080p50 entre processus. → Mesuré, ci-dessous.
- Compilation et `cargo deny` du job CEF sur les 2 OS. → Job CI `html`, `cargo deny` avec la
  feature `cef`.

## Mesures de l'essai (étape 4, 2026-11-10)
Sonde `vma-html --vma-probe` (crate `io-html`, feature `cef`, `src/bin/vma-html/probe.rs`) : une
page à fond transparent, pilotée par `SendExternalBeginFrame` à la cadence du maître. La page
compte ses `requestAnimationFrame` et écrit le compte dans un bloc ; chaque `OnPaint` est relu.
Sortie : un rapport, code 0 si tout tient. Machine de dev (Linux, release), CEF 156.0.26
(Chromium 156.0.8078.88), 1920 × 1080, 20 s par cadence après 25 ticks de chauffe, sans écran.

| Cadence | Images / ticks | Répétées / sautées | Pas de l'horloge de la page | Latence BeginFrame → OnPaint (p50 / p99) | CPU (un cœur) |
|---|---|---|---|---|---|
| 25 | 500 / 500 | 0 / 0 | 40 ms | 1,7 / 2,3 ms | 6 % |
| 29,97 | 599 / 599 | 0 / 0 | 33–34 ms | 1,7 / 2,4 ms | 7 % |
| 30 | 600 / 600 | 0 / 0 | 33–34 ms | 1,7 / 2,3 ms | 7 % |
| 50 | 1000 / 1000 | 0 / 0 | 20 ms | 1,3 / 2,3 ms | 9 % |
| 59,94 | 1198 / 1198 | 0 / 0 | 17 ms | 1,7 / 2,4 ms | 11 % |
| 60 | 1200 / 1200 | 0 / 0 | 16–17 ms | 1,3 / 2,1 ms | 11 % |

- **Alpha** : `rgba(255, 0, 0, 0.5)` relu (128, 0, 0, 128), `rgba(255, 255, 255, 0.25)` relu
  (64, 64, 64, 64), fond (0, 0, 0, 0) : BGRA **prémultiplié**, sans `background_color`.
- **Cadence** : l'horloge de la page (`rAF`) avance d'une période du maître par tick ; elle suit
  l'heure d'envoi du `BeginFrame` (retard d'envoi de notre boucle : p99 ≤ 0,2 ms).
- **Mémoire** : ≈ 115 Mio (PSS) par navigateur, ≈ 200 Mio pour le processus et ses
  sous-processus sans navigateur (8 processus en tout).
- **Copie** d'une image 1080p (7,9 Mio) : ≈ 0,7 ms (p99 0,9 ms), 396 Mio/s par input à 50 i/s.
- **Démarrage** : contexte CEF ≈ 40 ms, première image de la page ≈ 80 ms.

**CI (runs 42784279830, 42788150087 et 42791050693, 1080p50, sans écran)** :
- Ubuntu 24.04 : 1000 / 1000 sur 20 s, 3000 / 3000 sur 60 s, latence p99 ≤ 1,8 ms, ≈ 120 Mio par
  navigateur, 11–12 % d'un cœur.
- Windows : compile (wrapper C++ par cmake + Ninja), tourne sans écran, alpha prémultiplié.
  **3000 / 3000 sur 60 s après 5 s de chauffe** (pas de l'horloge 17–24 ms, latence p99 3,2 ms).
  Mais **une rafale de ticks sans image dans les ~3 premières secondes** après la création du
  navigateur, à 3 runs sur 4 : 6, 12 puis 7 ticks consécutifs (le processus de rendu figé
  140 à 255 ms, puis il reprend sans sauter d'étape), alors que nos `BeginFrame` partent à
  l'heure (retard ≤ 0,8 ms) ; rien sur la sortie d'erreur de Chromium. Cause non trouvée
  (démarrage de Chromium sous Windows, antivirus du runner ?). Non vu sous Linux.

Ce que l'essai a montré, et qui change la mise en œuvre (pas la décision) :
1. **`--run-all-compositor-stages-before-draw`** (option de Chromium pour le rendu déterministe
   du mode *headless*) : sans elle, le compositeur de Chromium produit l'image d'un tick sans
   attendre le thread principal de la page quand celui-ci est en retard. Vu à chaque essai vers
   la 200ᵉ image (V8 qui optimise le code de la page, probablement) : une image répète l'étape
   précédente. Avec elle, l'image attend la page (+ ≈ 1 ms de latence) : une page trop lente
   arrivera en retard au lieu de se répéter en silence, et le retard sera compté côté `vma`.
2. **`--ozone-platform=headless`** sous Linux, toujours : sans écran, Chromium ne démarre pas
   sans lui ; avec un écran (Wayland), il ouvre une connexion inutile et son processus GPU se
   plaint de Vulkan.
3. **Rendu logiciel (`--disable-gpu`) par défaut** : pour une page d'overlay, c'est le moins
   cher (le GPU de Chromium en Vulkan : 19 % de CPU et 5 ms de latence, par la relecture de
   l'image) et le plus simple (sans écran, l'initialisation GPU d'ANGLE échoue puis se rabat sur
   le logiciel). `--vma-gpu` garde le GPU pour comparer. À revoir avec une vraie page d'overlay
   (Q3) et pour le partage de texture (`OnAcceleratedPaint`, qui demande le GPU, § 7).
4. **Le relevé `/proc` de la sonde (≈ 10 ms) dans la boucle d'envoi** retardait un `BeginFrame`
   et en faisait perdre un : le thread qui envoie les `BeginFrame` ne doit rien faire d'autre
   (le futur thread `vma-html` côté application : `try_send` seulement).
5. **Distribution** : `libcef.so` fait 1,4 Gio avec ses symboles de débogage, 265 Mio
   strippé ; le paquet doit stripper. `vma-html` trouve `libcef.so` à côté de lui (rpath
   `$ORIGIN`, `build.rs` d'`io-html`). Fichiers à livrer : ceux que `cef-dll-sys` copie à côté
   du binaire (`.pak`, `icudtl.dat`, `v8_context_snapshot.bin`, `locales/`, SwiftShader).
6. **Licences** : le graphe de `vma-html` est permissif ; deux crates de compilation seulement
   (téléchargement et décompression de CEF) ont une licence hors liste, permissive, en
   exception dans `deny.toml` : `libbz2-rs-sys` (bzip2-1.0.6), `webpki-roots`
   (CDLA-Permissive-2.0).

## Options écartées
- **Lier CEF au binaire principal** : il ne démarrerait plus sans `libcef` (Linux, Windows),
  contraire à « absent → désactivé proprement » ; un plantage de CEF dans le processus
  principal ferait tomber le Program.
- **`cefsrc` (plugin GStreamer en C)** : à compiler à part, licence à vérifier, pas de
  contrôle de la cadence par le tick, données vers la page limitées.
- **WPE WebKit (`wpesrc`)** : Linux seulement.
- **Servo** : compatibilité insuffisante avec des overlays web existants.

## Conséquences
- Un nouveau binaire et un job CI plus lourd (téléchargement de CEF, cache) ; le reste de la CI
  est inchangé.
- Le format des messages entre `vma` et `vma-html` est interne (même version des deux
  binaires, vérifiée à la connexion).
- L'API de la Phase 5 n'aura qu'à traduire vers `HtmlData` / `HtmlScript`.
