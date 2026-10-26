# Installation de l'environnement de développement

La **Phase 0 n'a besoin que de Rust et d'un pilote GPU** (Vulkan sous Linux, D3D12 sous
Windows). GStreamer devient nécessaire à partir de la Phase 1 (`io-gst`).

## Rust
```sh
rustup toolchain install stable 1.95     # stable pour le dev, 1.95 = MSRV
cargo install --locked cargo-nextest cargo-deny
```

## Arch Linux (Omarchy)
```sh
# GPU : Vulkan (déjà présent avec les pilotes NVIDIA / Mesa)
sudo pacman -S --needed vulkan-icd-loader vulkan-tools
# Facultatif : rendu logiciel identique à la CI (lavapipe)
sudo pacman -S --needed vulkan-swrast
# GStreamer (Phase 1+)
sudo pacman -S --needed gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav
```
Vérifications :
```sh
gst-inspect-1.0 --version
gst-inspect-1.0 nvh264enc   # NVENC (Phase 2)
gst-inspect-1.0 rtmp2sink   # RTMP (Phase 2)
gst-inspect-1.0 srtsink     # SRT (Phase 2) — absent sur la machine de dev au 2026-10-26
```
NDI (`ndisrc`/`ndisink`, gst-plugins-rs) : procédure à documenter en Phase 3.

État constaté sur la machine de dev (2026-10-26) : GStreamer 1.28.6, `nvh264enc` et
`rtmp2sink` présents ; `srtsink`, `ndisrc` absents.

## Windows
1. Rust via `rustup-init.exe` (toolchain MSVC) + Visual Studio Build Tools (C++).
2. GStreamer (Phase 1+) : installer **les deux** paquets *MSVC 64-bit* (runtime **et**
   development) depuis <https://gstreamer.freedesktop.org/download/>, installation *Complete*.
3. Variables d'environnement (adapter le chemin) :
   ```powershell
   $env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = "C:\Program Files\gstreamer\1.0\msvc_x86_64\"
   $env:PATH += ";$env:GSTREAMER_1_0_ROOT_MSVC_X86_64\bin"
   $env:PKG_CONFIG_PATH = "$env:GSTREAMER_1_0_ROOT_MSVC_X86_64\lib\pkgconfig"
   ```
4. `pkg-config` : `winget install bloodrock.pkg-config-lite` (ou `choco install pkgconfiglite`).

## Lancer
```sh
cargo run                    # debug (dépendances optimisées)
cargo run --release
WGPU_POWER_PREF=low cargo run       # préférer l'iGPU (WGPU_ADAPTER_NAME ne vaut que pour les tests headless)
```
