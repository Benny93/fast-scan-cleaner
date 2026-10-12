"""Render the animated mascot (the app icon's hard drive with sunglasses).

    python3 -I assets/mascot/make.py OUT_DIR [--size 256] [--bg]

Writes OUT_DIR/frame_NN.png (FRAMES frames, MS ms each), OUT_DIR/mascot.gif and
OUT_DIR/sprite.png (all frames in an 8×8 grid, transparent; the app steps through it).
The app's copy: python3 -I assets/mascot/make.py assets/mascot --size 224 (then delete the frame_*.png and gif).
Needs rsvg-convert and Pillow.
"""
import math, subprocess, sys, pathlib
from PIL import Image

FRAMES, MS = 64, 50  # 3.2 s loop

def ease(x):  # smoothstep 0..1
    x = min(max(x, 0.0), 1.0)
    return x * x * (3 - 2 * x)

def window(t, a, b, fade):  # 0 outside [a, b], ramps up/down over `fade`
    return ease((t - a) / fade) * (1 - ease((t - (b - fade)) / fade))

def frame(t, bg):
    bob = 7 * math.sin(2 * math.pi * t)
    # Never lift the shades (eyes would spoil the cool): a slow nod dips them, then a smirk.
    nod = window(t, 0.34, 0.52, 0.07)
    lift = 10 * nod
    smirk = window(t, 0.40, 0.78, 0.08)
    blink = window(t, 0.86, 0.92, 0.02)        # LED dims
    glint = (t - 0.06) / 0.18                  # sweep across the lenses
    flare = window(t, 0.50, 0.70, 0.06)        # the big sparkle flares with the smirk
    twinkle = lambda ph: 0.75 + 0.25 * math.sin(2 * math.pi * (t * 2 + ph)) + 0.45 * flare * (ph == 0)
    glint_svg = ""
    if 0 <= glint <= 1:
        gx = -120 + 260 * glint
        glint_svg = (f'<g clip-path="url(#lenses)"><path d="M{300+gx} 540 l90 -170 l40 0 l-90 170z M{580+gx} 540 l90 -170 l40 0 l-90 170z" '
                     f'fill="#ffffff" opacity="0.45"/></g>')
    star = lambda x, y, r, color, ph: (
        f'<g transform="translate({x} {y}) scale({twinkle(ph):.3f})"><path d="M0 {-r} L{r*0.25} {-r*0.25} L{r} 0 L{r*0.25} {r*0.25} L0 {r} '
        f'L{-r*0.25} {r*0.25} L{-r} 0 L{-r*0.25} {-r*0.25}z" fill="{color}" opacity="{0.6 + 0.4 * twinkle(ph):.3f}"/></g>')
    background = '<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)"/>' if bg else ""
    lenses = ('<path d="M272 388 h202 q14 0 12 16 l-12 70 q-8 44 -56 44 h-96 q-44 0 -54 -44 l-10 -70 q-2 -16 14 -16z"/>'
              '<path d="M550 388 h202 q16 0 14 16 l-10 70 q-10 44 -54 44 h-96 q-48 0 -56 -44 l-12 -70 q-2 -16 12 -16z"/>')
    # Without the icon background, crop to the drive and its sparkles so the mascot fills the frame.
    view = "0 0 1024 1024" if bg else "170 190 710 710"
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view}">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#3b82f6"/><stop offset="1" stop-color="#1e3a8a"/></linearGradient>
    <linearGradient id="metal" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#f1f5f9"/><stop offset="1" stop-color="#cbd5e1"/></linearGradient>
    <linearGradient id="base" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#94a3b8"/><stop offset="1" stop-color="#64748b"/></linearGradient>
    <linearGradient id="lens" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#334155"/><stop offset="0.55" stop-color="#0f172a"/><stop offset="1" stop-color="#020617"/></linearGradient>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="150%"><feDropShadow dx="0" dy="18" stdDeviation="18" flood-color="#020617" flood-opacity="0.45"/></filter>
    <clipPath id="lenses">{lenses.replace("<path ", f'<path transform="translate(0 {lift:.1f})" ')}</clipPath>
  </defs>
  {background}
  {star(804, 264, 54, "#fde68a", 0.0)}
  {star(229, 835, 35, "#bfdbfe", 0.5)}
  <g transform="translate(0 {bob:.2f})">
    <g filter="url(#shadow)">
      <rect x="212" y="300" width="600" height="440" rx="72" fill="url(#metal)"/>
      <path d="M212 600 h600 v68 a72 72 0 0 1 -72 72 h-456 a72 72 0 0 1 -72 -72z" fill="url(#base)"/>
    </g>
    <rect x="260" y="642" width="300" height="20" rx="10" fill="#475569" opacity="0.55"/>
    <circle cx="742" cy="652" r="22" fill="#4ade80" opacity="{1 - 0.7 * blink:.3f}"/>
    <circle cx="736" cy="645" r="7" fill="#dcfce7" opacity="{1 - 0.7 * blink:.3f}"/>
    <path d="M444 {566 + 3 * nod:.1f} q70 {44 - 16 * smirk:.1f} 140 {-16 * smirk:.1f}" fill="none" stroke="#334155" stroke-width="18" stroke-linecap="round"/>
    <g transform="translate(0 {lift:.1f})">
      <path d="M232 405 L282 398" stroke="#0f172a" stroke-width="18" stroke-linecap="round"/>
      <path d="M792 405 L742 398" stroke="#0f172a" stroke-width="18" stroke-linecap="round"/>
      <g fill="url(#lens)" stroke="#020617" stroke-width="10">{lenses}</g>
      <path d="M484 402 q28 -18 56 0" fill="none" stroke="#020617" stroke-width="16" stroke-linecap="round"/>
      <g stroke="#ffffff" stroke-linecap="round" fill="none">
        <path d="M318 470 l44 -58" stroke-opacity="0.6" stroke-width="16"/><path d="M360 478 l26 -34" stroke-opacity="0.35" stroke-width="10"/>
        <path d="M596 470 l44 -58" stroke-opacity="0.6" stroke-width="16"/><path d="M638 478 l26 -34" stroke-opacity="0.35" stroke-width="10"/>
      </g>
    </g>
    {glint_svg}
  </g>
</svg>'''

def main():
    out = pathlib.Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=True)
    size = int(sys.argv[sys.argv.index("--size") + 1]) if "--size" in sys.argv else 256
    bg = "--bg" in sys.argv
    frames = []
    for i in range(FRAMES):
        png = out / f"frame_{i:02d}.png"
        subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", str(png)], input=frame(i / FRAMES, bg).encode(), check=True)
        frames.append(Image.open(png).convert("RGBA"))
    strip = Image.new("RGBA", (size * 8, size * FRAMES // 8))
    for i, f in enumerate(frames):
        strip.paste(f, (i % 8 * size, i // 8 * size))
    strip.save(out / "sprite.png", optimize=True)
    # GIF has 1-bit transparency: flatten onto the app's dark background unless the icon background is drawn.
    flat = [Image.alpha_composite(Image.new("RGBA", f.size, (10, 10, 10, 255)), f).convert("RGB") for f in frames]
    flat[0].save(out / "mascot.gif", save_all=True, append_images=flat[1:], duration=MS, loop=0, optimize=True)

if __name__ == "__main__":
    main()
