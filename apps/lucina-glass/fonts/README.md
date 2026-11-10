# Fonts

Subsets, so the kit embeds about 330 KB instead of 3.5 MB:

- Lato Thin, Light, Regular and Semibold (OFL 1.1, `LICENSE-Lato-OFL.txt`), from Debian's fonts-lato 2.0.
- DejaVu Sans Mono (Bitstream Vera licence, `LICENSE-DejaVu.txt`), from `apps/lucina-app/fonts`.

Made with fontTools (Python: the library that subsets fonts):

```
for f in Thin Light Regular Semibold; do
  python3 -m fontTools.subset /usr/share/fonts/truetype/lato/Lato-$f.ttf \
    --unicodes="U+0020-007E,U+00A0-00FF,U+2013,U+2014,U+2018,U+2019,U+201C,U+201D,U+2022,U+2026,U+00B7,U+2190-2193" \
    --layout-features="kern,liga,tnum,lnum" --output-file=Lato-$f.ttf
done
python3 -m fontTools.subset ../../../apps/lucina-app/fonts/DejaVuSansMono.ttf \
  --unicodes="U+0020-007E,U+00A0-00FF,U+2013,U+2014,U+2022,U+2026,U+00B7,U+2190-2193,U+2295" --output-file=DejaVuSansMono.ttf
```
