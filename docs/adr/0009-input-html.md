# ADR 0009 — Input HTML : CEF hors écran dans un processus auxiliaire

- **Statut** : **accepté** le 2026-11-09 par le mainteneur (avec la spec de la Phase 4)
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
  Xvfb ; CI Windows).
- `OnPaint` donne l'alpha d'une page à fond transparent, et sous quelle forme (prémultiplié
  attendu) ; `AlphaMode` de l'input en conséquence.
- **Une image par `BeginFrame`**, et `requestAnimationFrame` cadencé par eux (D5).
- Coût : CPU, mémoire par navigateur, copie 1080p50 entre processus.
- Compilation et `cargo deny` du job CEF sur les 2 OS.

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
