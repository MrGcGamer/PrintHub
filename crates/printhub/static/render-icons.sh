#!/bin/sh
# Renders every icon in this directory from logo.svg / logo-maskable.svg.
# Needs Inkscape and ImageMagick.
set -eu
cd "$(dirname "$0")"

inkscape logo.svg -o icon-192.png -w 192 -h 192
inkscape logo.svg -o icon-512.png -w 512 -h 512
inkscape logo-maskable.svg -o icon-maskable-512.png -w 512 -h 512
inkscape logo-maskable.svg -o apple-touch-icon.png -w 180 -h 180

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for s in 16 32 48; do inkscape logo.svg -o "$tmp/favicon-$s.png" -w $s -h $s; done
magick "$tmp/favicon-16.png" "$tmp/favicon-32.png" "$tmp/favicon-48.png" favicon.ico

magick identify favicon.ico apple-touch-icon.png icon-192.png icon-512.png icon-maskable-512.png
