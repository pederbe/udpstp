#!/bin/sh
# Rebuild the display face and the social image's self-contained fonts.
#
# usage: website/fonts/subset.sh   (needs npm, curl, tar, sha256sum and
#        fonttools with brotli: `pip install "fonttools[woff]==4.60.1"`)
#
# IBM Plex is under the SIL Open Font License 1.1 with the Reserved Font Name
# "Plex". A subset is a Modified Version, so the subset's family is renamed
# "udpstp Display", as the licence requires; OFL.txt beside it is the licence.
set -eu

version=2.0.0
out=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

(cd "$work" && npm pack --silent "@ibm/plex-sans-condensed@$version" >/dev/null && tar xzf ./*.tgz)
cp "$work/package/LICENSE.txt" "$out/OFL.txt"

pyftsubset "$work/package/fonts/complete/woff2/IBMPlexSansCondensed-Bold.woff2" \
  --unicodes="U+0020-007E,U+00A0-00FF,U+2013-2014,U+2018-201D,U+2022,U+2026,U+2192,U+2212" \
  --layout-features="kern,liga,tnum,lnum" \
  --flavor=woff2 --with-zopfli \
  --output-file="$work/subset.woff2"

# Rename the family (name IDs 1, 3, 4, 6, 16) so the subset doesn't carry the
# Reserved Font Name.
python3 - "$work/subset.woff2" "$out/udpstp-display-bold.woff2" <<'PY'
import sys
from fontTools.ttLib import TTFont
font = TTFont(sys.argv[1])
names = {1: 'udpstp Display', 3: 'udpstp Display Bold; subset of IBM Plex Sans Condensed Bold 2.0.0',
         4: 'udpstp Display Bold', 6: 'udpstpDisplay-Bold', 16: 'udpstp Display'}
for record in font['name'].names:
    if record.nameID in names:
        record.string = names[record.nameID]
font.flavor = 'woff2'
font.save(sys.argv[2])
PY
echo "wrote $out/udpstp-display-bold.woff2 and $out/OFL.txt"

curl -fsSL https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-fonts-ttf-2.37.tar.bz2 -o "$work/dejavu.tar.bz2"
echo "fa9ca4d13871dd122f61258a80d01751d603b4d3ee14095d65453b4e846e17d7  $work/dejavu.tar.bz2" | sha256sum -c -
tar xjf "$work/dejavu.tar.bz2" -C "$work"
cp "$work/dejavu-fonts-ttf-2.37/LICENSE" "$out/DejaVu-LICENSE.txt"
for pair in 'DejaVuSans:sans' 'DejaVuSansMono:mono' 'DejaVuSansMono-Bold:mono-bold'; do
  source=${pair%%:*}
  name=${pair#*:}
  pyftsubset "$work/dejavu-fonts-ttf-2.37/ttf/$source.ttf" \
    --unicodes="U+0020-007E,U+00A0" --flavor=woff2 --no-recalc-timestamp \
    --output-file="$out/udpstp-social-$name.woff2"
done
echo "wrote the social image fonts and DejaVu-LICENSE.txt"
