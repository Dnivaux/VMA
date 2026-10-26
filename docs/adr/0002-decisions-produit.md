# ADR 0002 — Décisions produit (BRIEF §12)

- **Statut** : accepté (décisions du mainteneur, 2026-10-26)
- **Date** : 2026-10-26
- **Contexte** : BRIEF §12 « Décisions ouvertes », à trancher avant la Phase 1

## Décisions

### UI : egui
Toute l'interface est en **egui** (eframe, backend wgpu), « pour l'instant ». Pas de Tauri /
React. Une UI web de contrôle à distance (tablette) reste possible plus tard **par-dessus
l'API native** (Phase 5) ; elle demandera alors son propre ADR.

### Nom : VMA (provisoire)
Le projet s'appelle **VMA** jusqu'au nom définitif. Appliqué partout : paquets `vma-*`,
binaire `vma`, titre de fenêtre, thread `vma-render`, variables `VMA_*`. `BRIEF.md` et le
rapport de soak du 2026-10-26 gardent l'ancien nom de code « Aiguillage » (documents
historiques). Avant le nom définitif : vérifier l'absence de conflit de marque (règle 2.2).

### Titres : HTML (CEF) **et** moteur natif
Deux moteurs de titres :
- **HTML via CEF** (`io-html`, Phase 4) : réutilise les overlays web existants (lower thirds,
  scores), données poussées par API sans rechargement ;
- **moteur natif** (texte + formes rendus en wgpu) : plus léger, sans navigateur embarqué.

Les deux produisent une source avec alpha, utilisable dans les overlay channels. Le moteur
natif demandera une spec et un ADR (rendu de texte : dépendance à choisir, licence à vérifier).
Phase : 4, avec les overlays, sauf demande contraire.

### Cadences : toutes, dès la Phase 1
L'outil final supporte toutes les cadences broadcast courantes. **Dès la Phase 1**, la cadence
de l'horloge maître est réglable parmi **25p, 29,97p, 30p, 50p, 59,94p, 60p**, et les inputs de
cadence quelconque sont rééchantillonnés sur l'horloge maître (BRIEF §5). Les critères
d'acceptation de la Phase 1 couvrent chacune de ces cadences maîtres (pas seulement 50p).
`FrameRate` étant rationnel depuis la Phase 0, `core` ne change pas.

Non couverts à ce stade, à confirmer si besoin : 23,976p / 24p, formats entrelacés (1080i50).

### macOS : plus tard
Ni code spécifique ni CI macOS pour l'instant. À reprendre par une décision explicite ;
d'ici là, ne rien introduire qui l'exclue (wgpu/Metal et eframe le supportent).

## Conséquences
- La spec de la Phase 1 inclut le changement de cadence maître et le rééchantillonnage, avec un
  test par cadence.
- Le soak de référence de la Phase 1 reste à 1080p50 ; des soaks courts valident les autres
  cadences.
