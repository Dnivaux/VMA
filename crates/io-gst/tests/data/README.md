# Fichiers de test avec alpha

Clips produits pour les tests (Phase 4, critère D3), sans contenu tiers : 64 × 36, 25 fps,
10 images d'un aplat R'G'B' (48, 128, 192) d'alpha 127 (0,5), droit. Générés par FFmpeg 8 :

```sh
L="color=c=0x3080C0@0.5:s=64x36:r=25:d=0.4"
ffmpeg -f lavfi -i "$L,format=yuva420p" -c:v libvpx-vp9 -pix_fmt yuva420p -auto-alt-ref 0 -b:v 0 -crf 10 alpha-vp9.webm
ffmpeg -f lavfi -i "$L,format=yuva420p" -c:v libvpx -pix_fmt yuva420p -auto-alt-ref 0 -b:v 1M alpha-vp8.webm
ffmpeg -f lavfi -i "$L,format=yuva444p10le" -c:v prores_ks -profile:v 4444 -pix_fmt yuva444p10le alpha-prores4444.mov
ffmpeg -f lavfi -i "$L,format=argb" -c:v qtrle -pix_fmt argb alpha-qtrle.mov
ffmpeg -f lavfi -i "$L,format=rgba" -c:v png -pix_fmt rgba alpha-png.mov
```

GStreamer les décode avec leur alpha : `vp8alphadecodebin` / `vp9alphadecodebin`
(`gst-plugins-bad`, A420), `avdec_prores` (A444_10LE), `avdec_qtrle` et `avdec_png`
(`gst-libav`, ARGB / RGBA).
