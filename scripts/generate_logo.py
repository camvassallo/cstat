"""Canonical Camalytics mark.

Concept: the faint curve is the shot's true flight path; the white dotted line
with nodes is the model fitting it, its samples straddling the curve above and
below like residuals around a trend.

VALUE HIERARCHY on the navy field, brightest first — this is the whole design:
    white        the fitted line + nodes    the star
    light blue   the basketball             the subject
    muted blue   the flight path            context; recedes

Handing the fit pure white and pushing the trajectory to muted blue separates
them by VALUE rather than only by weight, so the regression reads first at any
size — including 32px, where the arc is barely more than a smudge and the
white dots still carry the mark.

The arc is a TAPERED RIBBON: a filled shape built from the centreline's offset
curves, not a fixed-width stroke. That variable weight is what keeps it from
reading as clip art. Keeping it thin and muted also avoids the Orlando Magic
silhouette, which is a thick fully-opaque tapered comet streak with a ball on
the end.

Regenerate the asset set with `python final_mark.py <outdir>`, then rasterize:
rounded -> favicon/navbar (transparent corners), square -> apple-touch-icon
(iOS masks it itself and paints transparency black), bare -> the OG card.
"""
import math

P = [(70, 430), (118, 208), (268, 148), (404, 250)]
GHOST, GHOST_OP = "#6e9ec5", 0.34     # flight path
FIT = "#ffffff"                        # the regression
BALL_C, BALL_R = (404, 258), 38
NODE_XS   = [118, 168, 218, 268, 312]
NODE_OFFS = [35, -31, 29, -27, 23]
NODE_R = 12
CONNECT_R = 3.4      # connector dot radius
CONNECT_GAP = 18.0   # target spacing between connector dots
CLEAR = 8.0          # blank space kept around each node

DEFS = """  <defs>
    <linearGradient id="ball" x1="0" y1="0" x2="0.35" y2="1">
      <stop offset="0" stop-color="#a9d3f5"/><stop offset="1" stop-color="#5b8ab4"/>
    </linearGradient>
    <linearGradient id="field" x1="0" y1="0" x2="0.7" y2="1">
      <stop offset="0" stop-color="#08386e"/><stop offset="1" stop-color="#042545"/>
    </linearGradient>
  </defs>"""


def bez(t):
    (x0,y0),(x1,y1),(x2,y2),(x3,y3)=P; mt=1-t
    return (mt**3*x0+3*mt*mt*t*x1+3*mt*t*t*x2+t**3*x3,
            mt**3*y0+3*mt*mt*t*y1+3*mt*t*t*y2+t**3*y3)

def dbez(t):
    (x0,y0),(x1,y1),(x2,y2),(x3,y3)=P; mt=1-t
    return (3*mt*mt*(x1-x0)+6*mt*t*(x2-x1)+3*t*t*(x3-x2),
            3*mt*mt*(y1-y0)+6*mt*t*(y2-y1)+3*t*t*(y3-y2))

def t_for_x(xt):
    lo,hi=0.0,1.0
    for _ in range(60):
        m=(lo+hi)/2
        if bez(m)[0]<xt: lo=m
        else: hi=m
    return (lo+hi)/2

def ribbon(wmax=10, bias=0.30, n=170):
    top,bot=[],[]
    for i in range(n+1):
        t=i/n; x,y=bez(t); dx,dy=dbez(t)
        L=math.hypot(dx,dy) or 1.0
        nx,ny=-dy/L,dx/L
        w=wmax*(math.sin(math.pi*t)**0.62)*(1.0-bias*t)
        top.append((x+nx*w,y+ny*w)); bot.append((x-nx*w,y-ny*w))
    return "M "+" L ".join(f"{x:.1f} {y:.1f}" for x,y in top+bot[::-1])+" Z"

NODES = [(x, round(bez(t_for_x(x))[1]+o)) for x, o in zip(NODE_XS, NODE_OFFS)]

def seams(r, sw):
    k, c = r*0.70, r*0.245
    return (f'    <g stroke="#ffffff" stroke-width="{sw:.1f}" fill="none" '
            f'stroke-linecap="round" opacity="0.95">\n'
            f'      <path d="M {-r} 0 H {r}"/>\n'
            f'      <path d="M 0 {-r} V {r}"/>\n'
            f'      <path d="M {-k:.0f} {-k:.0f} C {-c:.0f} {-c*1.3:.0f}, {-c:.0f} {c*1.3:.0f}, {-k:.0f} {k:.0f}"/>\n'
            f'      <path d="M {k:.0f} {-k:.0f} C {c:.0f} {-c*1.3:.0f}, {c:.0f} {c*1.3:.0f}, {k:.0f} {k:.0f}"/>\n'
            f'    </g>')

def connector_dots():
    """Discrete dots between consecutive nodes, trimmed clear of both.

    A `stroke-dasharray` can't know where the nodes are, so its dots inevitably
    pile up against them and the line reads clumpy. Placing each dot explicitly
    lets us blank a fixed radius around every node and distribute what's left
    evenly, so the spacing stays regular and nothing collides.
    """
    out = []
    for (x0, y0), (x1, y1) in zip(NODES, NODES[1:]):
        dx, dy = x1 - x0, y1 - y0
        L = math.hypot(dx, dy)
        ux, uy = dx / L, dy / L
        trim = NODE_R + CLEAR
        span = L - 2 * trim
        if span <= 0:
            continue
        k = max(1, round(span / CONNECT_GAP))
        # k dots at the midpoints of k equal sub-spans -> symmetric, never
        # touching either endpoint.
        for i in range(k):
            d = trim + span * (i + 0.5) / k
            out.append((x0 + ux * d, y0 + uy * d))
    return out


def art():
    bx, by = BALL_C
    conn = "".join(
        f'\n    <circle cx="{x:.1f}" cy="{y:.1f}" r="{CONNECT_R}" fill="{FIT}"/>'
        for x, y in connector_dots())
    dots = "".join(
        f'\n    <circle cx="{x}" cy="{y}" r="{NODE_R}" fill="{FIT}"/>'
        for x, y in NODES)
    return (f'  <path d="{ribbon()}" fill="{GHOST}" opacity="{GHOST_OP}"/>\n'
            f'  <g transform="translate({bx},{by})">\n'
            f'    <circle cx="0" cy="0" r="{BALL_R}" fill="url(#ball)"/>\n'
            f'{seams(BALL_R, BALL_R*0.075)}\n  </g>\n'
            f'  <g>{conn}</g>\n'
            f'  <g>{dots}</g>')


def svg(rx=96, field=True):
    rect = f'  <rect width="512" height="512" rx="{rx}" fill="url(#field)"/>\n' if field else ''
    return ("<!--\n" + __doc__.strip() + "\n-->\n"
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" '
            'width="512" height="512" role="img" aria-label="Camalytics">\n'
            f'{DEFS}\n{rect}{art()}\n</svg>\n')

if __name__ == "__main__":
    import sys
    d = sys.argv[1]
    open(f"{d}/final_rounded.svg","w").write(svg(96, True))
    open(f"{d}/final_square.svg","w").write(svg(0, True))
    open(f"{d}/final_bare.svg","w").write(svg(0, False))
    print("wrote final_{rounded,square,bare}.svg")
