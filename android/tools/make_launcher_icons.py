#!/usr/bin/env python3
"""Draw the launcher icon into the app's mipmap directories.

The icon is generated rather than committed as an opaque blob: a yellow rounded
square (the screen bezel), a dark screen, and a face drawn on it.  Everything is
plain arithmetic plus zlib, so this runs anywhere Python does and needs no image
library.

    python3 android/tools/make_launcher_icons.py
"""

import math
import pathlib
import struct
import zlib

# Density bucket -> icon edge in pixels.
SIZES = {
    "mdpi": 48,
    "hdpi": 72,
    "xhdpi": 96,
    "xxhdpi": 144,
    "xxxhdpi": 192,
}

BEZEL_TOP = (0xFF, 0xD2, 0x1E)
BEZEL_BOTTOM = (0xFF, 0xA5, 0x0A)
SCREEN = (0x10, 0x12, 0x1A)
FACE = (0xFF, 0xD2, 0x1E)

SAMPLES = 3  # supersampling factor per axis


def rounded_box(x, y, left, top, right, bottom, radius):
    """Signed coverage test for a rounded rectangle (True = inside)."""
    cx = min(max(x, left + radius), right - radius)
    cy = min(max(y, top + radius), bottom - radius)
    return math.hypot(x - cx, y - cy) <= radius


def shade(x, y, size):
    """Colour of the icon at a point, or None where it is transparent."""
    unit = size / 100.0

    if not rounded_box(x, y, 2 * unit, 2 * unit, 98 * unit, 98 * unit, 22 * unit):
        return None

    # Bezel, with a soft vertical gradient so the icon is not a flat slab.
    t = (y / size) ** 0.9
    colour = tuple(
        round(BEZEL_TOP[i] + (BEZEL_BOTTOM[i] - BEZEL_TOP[i]) * t) for i in range(3)
    )

    inside_screen = rounded_box(x, y, 14 * unit, 18 * unit, 86 * unit, 82 * unit, 12 * unit)
    if not inside_screen:
        return colour
    colour = SCREEN

    # Eyes.
    for eye_x in (38 * unit, 62 * unit):
        if math.hypot(x - eye_x, y - 41 * unit) <= 7 * unit:
            return FACE

    # Smile: the lower half of a ring.
    distance = math.hypot(x - 50 * unit, y - 46 * unit)
    if 18 * unit <= distance <= 24 * unit and y >= 52 * unit:
        return FACE

    return colour


def render(size):
    """RGBA rows for one icon size."""
    rows = []
    step = 1.0 / SAMPLES
    offset = step / 2.0
    for py in range(size):
        row = bytearray()
        for px in range(size):
            red = green = blue = alpha = 0
            for sy in range(SAMPLES):
                for sx in range(SAMPLES):
                    colour = shade(px + offset + sx * step, py + offset + sy * step, size)
                    if colour is not None:
                        red += colour[0]
                        green += colour[1]
                        blue += colour[2]
                        alpha += 255
            total = SAMPLES * SAMPLES
            if alpha == 0:
                row += bytes((0, 0, 0, 0))
                continue
            covered = alpha / 255.0
            row += bytes(
                (
                    round(red / covered),
                    round(green / covered),
                    round(blue / covered),
                    round(alpha / total),
                )
            )
        rows.append(bytes(row))
    return rows


def png(rows, size):
    """Encode RGBA rows as a PNG file."""
    raw = b"".join(b"\x00" + row for row in rows)

    def chunk(kind, payload):
        return (
            struct.pack(">I", len(payload))
            + kind
            + payload
            + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def main():
    res = pathlib.Path(__file__).resolve().parent.parent / "app" / "src" / "main" / "res"
    for density, size in SIZES.items():
        directory = res / ("mipmap-" + density)
        directory.mkdir(parents=True, exist_ok=True)
        target = directory / "ic_launcher.png"
        target.write_bytes(png(render(size), size))
        print("wrote {} ({}x{}, {} bytes)".format(target, size, size, target.stat().st_size))


if __name__ == "__main__":
    main()
