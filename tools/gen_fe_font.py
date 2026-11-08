#!/usr/bin/env python3
"""Build the FE-Schrift face the SSTV picture text can be drawn in.

    python3 tools/gen_fe_font.py FE-Schrift.svg crates/sdroxide-ui/assets/fonts/FESchrift-sdroxide.otf

The outlines come from Wikimedia Commons' "File:FE-Schrift.svg"
(https://commons.wikimedia.org/wiki/File:FE-Schrift.svg): the glyphs of the
German number-plate typeface (designed by Karlgeorg Hoefer for the
Bundesanstalt für Straßenwesen), vectorised by Liftarn and marked there as
public domain. No digital FE-Schrift with a licence that allows bundling it was
found, so this one is made from that drawing and released under CC0 1.0.

The drawing has 39 glyphs in three rows — A–Z, then 0–9 and Ä Ö Ü — one SVG
path each. They are placed on a common baseline, scaled to a cap height of 700
units in a 1000-unit em (Chakra Petch's proportions, so a layer keeps its size
when its face is changed), and spaced with an even side bearing. Lower case
maps onto the capitals, as on a plate; a space, a hyphen and a full stop are
drawn here from the stroke the capitals use. Everything else is left to the
fallback face the compositor puts behind it.

Needs fontTools. Written to CFF (cubic) so the outlines are kept exactly.
"""

import sys
import xml.etree.ElementTree as ET

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.basePen import BasePen
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.recordingPen import RecordingPen
from fontTools.pens.reverseContourPen import ReverseContourPen
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.pens.transformPen import TransformPen
from fontTools.svgLib.path import parse_path

ROWS = ["ABCDEFGHIJKLM", "NOPQRSTUVWXYZ", "0123456789ÄÖÜ"]
UPM, CAP, ASC, DESC = 1000, 700, 992, -308
SIDE = 55  # side bearing, font units
NAME = "FE-Schrift sdroxide"
PS_NAME = "FESchrift-sdroxide"
SVG_NS = "{http://www.w3.org/2000/svg}"


class Flatten(BasePen):
    """Each contour as a polygon, curves cut into short straight pieces."""

    def __init__(self):
        super().__init__(None)
        self.polys, self.cur = [], []

    def _moveTo(self, p):
        self.cur = [p]

    def _lineTo(self, p):
        self.cur.append(p)

    def _curveToOne(self, p1, p2, p3):
        p0 = self.cur[-1]
        for i in range(1, 17):
            t = i / 16
            mt = 1 - t
            self.cur.append(
                tuple(
                    mt**3 * a + 3 * mt * mt * t * b + 3 * mt * t * t * c + t**3 * d
                    for a, b, c, d in zip(p0, p1, p2, p3)
                )
            )

    def _qCurveToOne(self, p1, p2):
        p0 = self.cur[-1]
        for i in range(1, 17):
            t = i / 16
            mt = 1 - t
            self.cur.append(tuple(mt * mt * a + 2 * mt * t * b + t * t * c for a, b, c in zip(p0, p1, p2)))

    def _closePath(self):
        if self.cur:
            self.polys.append(self.cur)
        self.cur = []

    _endPath = _closePath


def area(poly):
    return sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(poly, poly[1:] + poly[:1])) / 2


def inside(pt, poly):
    x, y = pt
    hit = False
    for (x0, y0), (x1, y1) in zip(poly, poly[1:] + poly[:1]):
        if (y0 > y) != (y1 > y) and x < x0 + (y - y0) * (x1 - x0) / (y1 - y0):
            hit = not hit
    return hit


def split_contours(rec):
    """A recording's contours, one recording each."""
    out, cur = [], None
    for op, args in rec.value:
        if op == "moveTo":
            cur = RecordingPen()
        cur.value.append((op, args))
        if op in ("closePath", "endPath"):
            out.append(cur)
    return out


def normalised(rec):
    """The glyph with outer contours counter-clockwise and holes clockwise
    (y up), what CFF expects — whichever way the drawing went round them."""
    contours = split_contours(rec)
    polys = []
    for c in contours:
        f = Flatten()
        c.replay(f)
        polys.append(f.polys[0])
    out = RecordingPen()
    for i, (c, poly) in enumerate(zip(contours, polys)):
        depth = sum(1 for j, other in enumerate(polys) if j != i and inside(poly[0], other))
        want_ccw = depth % 2 == 0
        if (area(poly) > 0) != want_ccw:
            c.replay(ReverseContourPen(out))
        else:
            c.replay(out)
    return out


def load(svg_path):
    """The 39 paths, in SVG coordinates (y down), each as a recording."""
    root = ET.parse(svg_path).getroot()
    group = root.find(f"{SVG_NS}g")
    tx, ty = 0.0, 0.0
    t = group.get("transform", "")
    if t.startswith("translate("):
        tx, ty = (float(v) for v in t[len("translate(") : -1].split(","))
    glyphs = []
    for p in group.iter(f"{SVG_NS}path"):
        rec = RecordingPen()
        parse_path(p.get("d"), TransformPen(rec, (1, 0, 0, 1, tx, ty)))
        b = BoundsPen(None)
        rec.replay(b)
        glyphs.append((rec, b.bounds))
    if len(glyphs) != sum(map(len, ROWS)):
        sys.exit(f"expected {sum(map(len, ROWS))} glyphs, found {len(glyphs)}")
    return glyphs


def stem(rec, y):
    """The width of the first stroke a horizontal line at `y` crosses."""
    f = Flatten()
    rec.replay(f)
    xs = []
    for poly in f.polys:
        for (x0, y0), (x1, y1) in zip(poly, poly[1:] + poly[:1]):
            if (y0 > y) != (y1 > y):
                xs.append(x0 + (y - y0) * (x1 - x0) / (y1 - y0))
    xs.sort()
    return xs[1] - xs[0]


def main(svg_path, out_path):
    glyphs = load(svg_path)
    # Rows by where each glyph's middle sits, then left to right.
    glyphs.sort(key=lambda g: (g[1][1] + g[1][3]) / 2)
    rows = [glyphs[i * 13 : (i + 1) * 13] for i in range(3)]
    placed = {}
    for chars, row in zip(ROWS, rows):
        row.sort(key=lambda g: g[1][0])
        # The baseline is where the letters stand: the commonest bottom edge.
        bottoms = sorted(b[3] for _, b in row)
        base = bottoms[len(bottoms) // 2]
        # Cap height from the tallest plain letter of the row (umlauts carry
        # their dots above it, so the median top is taken instead).
        tops = sorted(b[1] for _, b in row)
        top = tops[len(tops) // 2]
        for ch, (rec, b) in zip(chars, row):
            placed[ch] = (rec, b, base, base - top)
    # One scale for every glyph, from the letters' own height.
    cap_svg = placed["H"][3]
    k = CAP / cap_svg

    fb = FontBuilder(UPM, isTTF=False)
    names, cmap, charstrings, metrics = [".notdef", "space"], {0x20: "space"}, {}, {}

    def put(name, rec, xmin, xmax, base):
        pen = T2CharStringPen(0, None)
        width = round((xmax - xmin) * k) + 2 * SIDE
        # SVG y runs down; the font's runs up from the baseline.
        moved = RecordingPen()
        rec.replay(TransformPen(moved, (k, 0, 0, -k, SIDE - xmin * k, base * k)))
        normalised(moved).replay(pen)
        charstrings[name] = pen.getCharString()
        metrics[name] = (width, SIDE)

    for ch, (rec, b, base, _) in placed.items():
        name = {"Ä": "Adieresis", "Ö": "Odieresis", "Ü": "Udieresis"}.get(ch, ch)
        if ch.isdigit():
            name = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"][int(ch)]
        names.append(name)
        put(name, rec, b[0], b[2], base)
        cmap[ord(ch)] = name
        if ch.isalpha():
            cmap[ord(ch.lower())] = name

    # The stroke the capitals are drawn with, for the few marks drawn here.
    rec_h, b_h, base_h, _ = placed["H"]
    weight = round(stem(rec_h, (b_h[1] + b_h[3]) / 2 + (b_h[3] - b_h[1]) * 0.25) * k)

    def box(name, w, h, y):
        pen = T2CharStringPen(0, None)
        pen.moveTo((SIDE, y))
        pen.lineTo((SIDE + w, y))
        pen.lineTo((SIDE + w, y + h))
        pen.lineTo((SIDE, y + h))
        pen.closePath()
        charstrings[name] = pen.getCharString()
        metrics[name] = (w + 2 * SIDE, SIDE)
        names.append(name)

    box("hyphen", round(CAP * 0.42), weight, round(CAP * 0.5 - weight / 2))
    cmap[ord("-")] = "hyphen"
    box("period", weight, weight, 0)
    cmap[ord(".")] = "period"

    empty = T2CharStringPen(0, None)
    charstrings["space"] = empty.getCharString()
    metrics["space"] = (round(CAP * 0.45), 0)
    notdef = T2CharStringPen(0, None)
    charstrings[".notdef"] = notdef.getCharString()
    metrics[".notdef"] = (round(CAP * 0.6), 0)

    fb.setupGlyphOrder(names)
    fb.setupCharacterMap(cmap)
    fb.setupCFF(PS_NAME, {"FullName": NAME, "FamilyName": NAME}, charstrings, {})
    fb.setupHorizontalMetrics(metrics)
    fb.setupHorizontalHeader(ascent=ASC, descent=DESC)
    fb.setupNameTable(
        {
            "familyName": NAME,
            "styleName": "Regular",
            "uniqueFontIdentifier": PS_NAME,
            "fullName": NAME,
            "psName": PS_NAME,
            "version": "Version 1.000",
            "copyright": "No rights reserved. Outlines from Wikimedia Commons File:FE-Schrift.svg (public domain).",
            "licenseDescription": "CC0 1.0 Universal: https://creativecommons.org/publicdomain/zero/1.0/",
            "licenseInfoURL": "https://creativecommons.org/publicdomain/zero/1.0/",
        }
    )
    fb.setupOS2(
        sTypoAscender=ASC,
        sTypoDescender=DESC,
        usWinAscent=ASC,
        usWinDescent=-DESC,
        sCapHeight=CAP,
        sxHeight=CAP,
        fsType=0,
    )
    fb.setupPost()
    # A fixed date rather than the build's, so the same drawing always builds
    # the same bytes. Seconds from 1904-01-01 (the format's epoch) to 2026-11-08.
    fb.font["head"].created = fb.font["head"].modified = 3876940800
    fb.font.recalcTimestamp = False
    fb.save(out_path)
    print(f"{out_path}: {len(names)} glyphs, {len(cmap)} characters, stroke {weight}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
